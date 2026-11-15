//! Build a SPHEREx cutout from byte ranges of a full-frame L2 file.
//!
//! The IRSA cutout service (`/ibe/...?center=&size=`) streamed ~4 MB per
//! 0.05 deg cutout at ~55 KB/s on 2026-11-14 (73 s each), 97% of it the
//! unchanged EPSF table. The same L2 files are mirrored in the public AWS
//! bucket `nasa-irsa-spherex`, which honours HTTP `Range`. This module
//! holds the pure FITS logic to rebuild exactly what the cutout service
//! returns from a few ranges of the parent file:
//!
//! - [`read_header`]: parse one HDU header from the start of a buffer;
//! - [`cut_box`]: the pixel box covering `(ra, dec, size)` via the sky WCS
//!   (TAN + SIP), clipped to the array;
//! - [`crop_rows`]: column crop of full-width image rows;
//! - [`decode_flag_rows`]: decompress only the RICE row-tiles of FLAGS
//!   that the box needs;
//! - [`cutout_header`] / [`flags_image_header`] / [`assemble`]: headers
//!   with `NAXISn` and every `CRPIXn`, `CRPIXnA`, `CRPIXnW` shifted, as the
//!   cutout service does (verified key-by-key against a real cutout).
//!
//! The output is a normal MEF that [`super::parse_frame`] reads.

use super::compimg::{self, Pixels};
use super::error::FitsError;
use super::hdu::{declared_data_len, Hdu};
use super::header::{CardValue, Header};
use super::wcs::SkyWcs;

const BLOCK: usize = 2880;
const CARD: usize = 80;

/// One parsed HDU header plus where its data lives.
#[derive(Debug, Clone)]
pub struct HeaderBlock {
    /// Raw header bytes including the END card, padded to 2880.
    pub bytes: Vec<u8>,
    /// Parsed cards.
    pub header: Header,
    /// Declared (unpadded) data length in bytes.
    pub data_len: usize,
}

impl HeaderBlock {
    /// Padded data length (distance from data start to the next HDU).
    pub fn padded_data_len(&self) -> usize {
        self.data_len.div_ceil(BLOCK) * BLOCK
    }

    /// `EXTNAME`, trimmed (`None` for the primary HDU).
    pub fn extname(&self) -> Option<String> {
        self.header.opt_str("EXTNAME").map(|s| s.trim().to_string())
    }
}

/// Parse the HDU header at the start of `buf`. `Ok(None)` when `buf`
/// ends before the END card (the caller should read more bytes).
pub fn read_header(buf: &[u8]) -> Result<Option<HeaderBlock>, FitsError> {
    let mut header = Header::default();
    let mut pos = 0usize;
    while pos + BLOCK <= buf.len() {
        for i in 0..BLOCK / CARD {
            let card = &buf[pos + i * CARD..pos + (i + 1) * CARD];
            let (key, value) = Header::parse_card(card);
            if key == "END" {
                let len = pos + BLOCK;
                let data_len = declared_data_len(&header)?;
                return Ok(Some(HeaderBlock {
                    bytes: buf[..len].to_vec(),
                    header,
                    data_len,
                }));
            }
            header.cards.push((key, value));
        }
        pos += BLOCK;
    }
    Ok(None)
}

/// 0-based pixel box `[x0, x0 + nx) x [y0, y0 + ny)` inside the array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutBox {
    /// First column (0-based).
    pub x0: usize,
    /// First row (0-based, FITS order: row 0 is the first row in the file).
    pub y0: usize,
    /// Number of columns.
    pub nx: usize,
    /// Number of rows.
    pub ny: usize,
}

/// Box of side `round(size_deg / pixel_scale)` centred on the pixel that
/// contains `(ra, dec)`, clipped to the `NAXIS1 x NAXIS2` array of the
/// image header. `Ok(None)` when the box misses the array (the cutout
/// service answers HTTP 422 "Arrays do not overlap" there).
pub fn cut_box(
    image: &Header,
    ra_deg: f64,
    dec_deg: f64,
    size_deg: f64,
) -> Result<Option<CutBox>, FitsError> {
    let wcs = SkyWcs::from_header(image)?;
    let naxis1 = image.req_i64("NAXIS1")?.max(0) as usize;
    let naxis2 = image.req_i64("NAXIS2")?.max(0) as usize;
    let cd = wcs.cd;
    let scale = (cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]).abs().sqrt();
    if !(scale.is_finite() && scale > 0.0 && size_deg.is_finite() && size_deg > 0.0) {
        return Err(FitsError::Corrupt("degenerate WCS scale or size".into()));
    }
    let n = ((size_deg / scale).round() as i64).max(1);
    let Ok((px, py)) = wcs.radec_to_pixel(ra_deg, dec_deg) else {
        return Ok(None);
    };
    if !(px.is_finite() && py.is_finite()) {
        return Ok(None);
    }
    // 1-based first pixel so that the centre pixel sits in the middle.
    let first = |p: f64| p.round() as i64 - (n - 1) / 2;
    let clip = |start1: i64, len: usize| -> Option<(usize, usize)> {
        let lo = start1.max(1);
        let hi = (start1 + n - 1).min(len as i64);
        (hi >= lo).then(|| ((lo - 1) as usize, (hi - lo + 1) as usize))
    };
    let (Some((x0, nx)), Some((y0, ny))) = (clip(first(px), naxis1), clip(first(py), naxis2))
    else {
        return Ok(None);
    };
    Ok(Some(CutBox { x0, y0, nx, ny }))
}

/// Byte range (relative to the HDU data start) of the full-width rows
/// `[b.y0, b.y0 + b.ny)` of an uncompressed image HDU.
pub fn row_range(image: &Header, b: &CutBox) -> Result<std::ops::Range<usize>, FitsError> {
    let bpp = (image.req_i64("BITPIX")?.unsigned_abs() / 8) as usize;
    let row = bpp * image.req_i64("NAXIS1")?.max(0) as usize;
    Ok(b.y0 * row..(b.y0 + b.ny) * row)
}

/// Crop columns `[b.x0, b.x0 + b.nx)` from `b.ny` full-width rows
/// (big-endian pixels copied verbatim).
pub fn crop_rows(image: &Header, rows: &[u8], b: &CutBox) -> Result<Vec<u8>, FitsError> {
    let bpp = (image.req_i64("BITPIX")?.unsigned_abs() / 8) as usize;
    let row = bpp * image.req_i64("NAXIS1")?.max(0) as usize;
    if rows.len() < row * b.ny || (b.x0 + b.nx) * bpp > row {
        return Err(FitsError::Truncated("cutout rows"));
    }
    let mut out = Vec::with_capacity(b.nx * b.ny * bpp);
    for r in 0..b.ny {
        let start = r * row + b.x0 * bpp;
        out.extend_from_slice(&rows[start..start + b.nx * bpp]);
    }
    Ok(out)
}

/// Layout of a row-tiled compressed image (`ZTILE1 = ZNAXIS1`,
/// `ZTILE2 = 1`), the only layout [`decode_flag_rows`] supports.
#[derive(Debug, Clone, Copy)]
pub struct TileRows {
    /// Bytes per table row (`NAXIS1`).
    pub row_len: usize,
    /// Offset of the heap from the data start (`THEAP`, default the
    /// end of the main table).
    pub heap_offset: usize,
    /// Descriptor is `Q` (2 x int64) rather than `P` (2 x int32).
    pub q_descriptor: bool,
}

/// Check that `h` is a row-tiled compressed image and describe it.
pub fn tile_rows(h: &Header) -> Result<TileRows, FitsError> {
    if h.opt_bool("ZIMAGE") != Some(true) {
        return Err(FitsError::Corrupt("FLAGS is not a compressed image".into()));
    }
    let znaxis1 = h.req_i64("ZNAXIS1")?;
    let tile1 = h.opt_i64("ZTILE1").unwrap_or(znaxis1);
    let tile2 = h.opt_i64("ZTILE2").unwrap_or(1);
    if tile1 != znaxis1 || tile2 != 1 || h.req_i64("ZNAXIS")? != 2 {
        return Err(FitsError::Corrupt("FLAGS tiles are not single rows".into()));
    }
    let row_len = h.req_i64("NAXIS1")?.max(0) as usize;
    let nrows = h.req_i64("NAXIS2")?.max(0) as usize;
    let tform = h.req_str("TFORM1")?.to_ascii_uppercase();
    let q_descriptor = tform.contains('Q');
    let heap_offset = h
        .opt_i64("THEAP")
        .map_or(row_len * nrows, |v| v.max(0) as usize);
    Ok(TileRows {
        row_len,
        heap_offset,
        q_descriptor,
    })
}

/// `(count, offset)` heap descriptors of the first column for each table
/// row in `rows` (raw main-table bytes of consecutive rows).
pub fn descriptors(t: &TileRows, rows: &[u8]) -> Result<Vec<(usize, usize)>, FitsError> {
    if t.row_len == 0 || !rows.len().is_multiple_of(t.row_len) {
        return Err(FitsError::Truncated("compressed table rows"));
    }
    let mut out = Vec::with_capacity(rows.len() / t.row_len);
    for r in rows.chunks(t.row_len) {
        let (count, offset) = if t.q_descriptor {
            let a = r.get(0..8).ok_or(FitsError::Truncated("Q descriptor"))?;
            let b = r.get(8..16).ok_or(FitsError::Truncated("Q descriptor"))?;
            (be_u64(a), be_u64(b))
        } else {
            let a = r.get(0..4).ok_or(FitsError::Truncated("P descriptor"))?;
            let b = r.get(4..8).ok_or(FitsError::Truncated("P descriptor"))?;
            (be_u64(a), be_u64(b))
        };
        out.push((count as usize, offset as usize));
    }
    Ok(out)
}

fn be_u64(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, x| (acc << 8) | u64::from(*x))
}

/// Decompress the FLAGS row-tiles of `b` and crop them to the box.
///
/// `rows` holds the main-table rows `[b.y0, b.y0 + b.ny)`; `heap` holds
/// heap bytes starting at heap offset `heap_base`, covering every
/// descriptor of those rows. Returns `b.nx * b.ny` pixels, row-major.
pub fn decode_flag_rows(
    h: &Header,
    rows: &[u8],
    heap: &[u8],
    heap_base: usize,
    b: &CutBox,
) -> Result<Vec<i32>, FitsError> {
    let t = tile_rows(h)?;
    let descs = descriptors(&t, rows)?;
    if descs.len() != b.ny {
        return Err(FitsError::Truncated("compressed table rows"));
    }
    // Synthetic HDU: the same compressed layout with only `b.ny` rows,
    // descriptors rebased onto the fetched heap slice.
    let mut table = Vec::with_capacity(rows.len() + heap.len());
    for (k, (count, offset)) in descs.iter().enumerate() {
        let off = offset
            .checked_sub(heap_base)
            .filter(|o| o + count <= heap.len())
            .ok_or(FitsError::Truncated("compressed heap"))?;
        let r = &rows[k * t.row_len..(k + 1) * t.row_len];
        let mut row = r.to_vec();
        if t.q_descriptor {
            row[8..16].copy_from_slice(&(off as u64).to_be_bytes());
        } else {
            let off32 = u32::try_from(off).map_err(|_| FitsError::Corrupt("heap offset".into()))?;
            row[4..8].copy_from_slice(&off32.to_be_bytes());
        }
        table.extend_from_slice(&row);
    }
    table.extend_from_slice(heap);

    let mut header = h.clone();
    header.cards.retain(|(k, _)| k != "THEAP");
    set_int(&mut header, "NAXIS2", b.ny as i64);
    set_int(&mut header, "ZNAXIS2", b.ny as i64);
    set_int(&mut header, "PCOUNT", heap.len() as i64);
    let hdu = Hdu {
        header,
        data: 0..table.len(),
        is_primary: false,
    };
    let img = compimg::decompress(&hdu, &table)?;
    let width = img.dims.first().copied().unwrap_or(0);
    let vals: Vec<i64> = match img.pixels {
        Pixels::Int(v) => v,
        Pixels::Float(v) => v.into_iter().map(|x| x as i64).collect(),
    };
    if b.x0 + b.nx > width || vals.len() < width * b.ny {
        return Err(FitsError::Truncated("decompressed FLAGS rows"));
    }
    let mut out = Vec::with_capacity(b.nx * b.ny);
    for r in 0..b.ny {
        for x in b.x0..b.x0 + b.nx {
            out.push(vals[r * width + x] as i32);
        }
    }
    Ok(out)
}

fn set_int(h: &mut Header, key: &str, v: i64) {
    for (k, val) in &mut h.cards {
        if k == key {
            *val = Some(CardValue::Int(v));
            return;
        }
    }
    h.cards.push((key.to_string(), Some(CardValue::Int(v))));
}

/// Format one 80-byte `KEY     = value` card.
fn value_card(key: &str, value: &str) -> [u8; CARD] {
    let text = format!("{key:<8}= {value:>20}");
    let mut card = [b' '; CARD];
    for (i, c) in text.bytes().take(CARD).enumerate() {
        card[i] = c;
    }
    card
}

/// Shifted reference pixel: `crpix - origin` (0-based origin).
fn shifted(h: &Header, key: &str, origin: usize) -> Option<String> {
    h.opt_f64(key).map(|v| format!("{:?}", v - origin as f64))
}

/// Copy of the raw header `bytes` with `NAXIS1/2 = nx/ny` and every
/// `CRPIXn`, `CRPIXnA`, `CRPIXnW` shifted by the box origin; cards in
/// `drop` are removed and `extra` cards replace the leading structural
/// ones when given (`(key, value)` pairs written in order first).
fn rewrite(
    bytes: &[u8],
    h: &Header,
    b: &CutBox,
    drop: &dyn Fn(&str) -> bool,
    lead: &[(&str, String)],
) -> Vec<u8> {
    let mut set: Vec<(String, String)> = vec![
        ("NAXIS1".into(), b.nx.to_string()),
        ("NAXIS2".into(), b.ny.to_string()),
    ];
    for (axis, origin) in [("1", b.x0), ("2", b.y0)] {
        for suffix in ["", "A", "W"] {
            let key = format!("CRPIX{axis}{suffix}");
            if let Some(v) = shifted(h, &key, origin) {
                set.push((key, v));
            }
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    for (k, v) in lead {
        out.extend_from_slice(&value_card(k, v));
    }
    for card in bytes.chunks(CARD) {
        if card.len() < CARD {
            break;
        }
        let key = std::str::from_utf8(&card[..8]).unwrap_or("").trim_end();
        if key == "END" {
            break;
        }
        if lead.iter().any(|(k, _)| *k == key) || drop(key) {
            continue;
        }
        if let Some((k, v)) = set.iter().find(|(k, _)| k == key) {
            out.extend_from_slice(&value_card(k, v));
        } else {
            out.extend_from_slice(card);
        }
    }
    let mut end = [b' '; CARD];
    end[..3].copy_from_slice(b"END");
    out.extend_from_slice(&end);
    out.resize(out.len().div_ceil(BLOCK) * BLOCK, b' ');
    out
}

/// Header of a cropped uncompressed image HDU (IMAGE, VARIANCE, ZODI).
pub fn cutout_header(block: &HeaderBlock, b: &CutBox) -> Vec<u8> {
    rewrite(&block.bytes, &block.header, b, &|_| false, &[])
}

/// Header of the FLAGS HDU as the cutout service emits it: an
/// uncompressed int32 IMAGE extension carrying every non-structural card
/// of the compressed header (legend, WCS), with shifted WCS.
pub fn flags_image_header(block: &HeaderBlock, b: &CutBox) -> Vec<u8> {
    let structural = |k: &str| {
        matches!(
            k,
            "XTENSION"
                | "BITPIX"
                | "NAXIS"
                | "NAXIS1"
                | "NAXIS2"
                | "PCOUNT"
                | "GCOUNT"
                | "TFIELDS"
                | "THEAP"
        ) || is_compression_key(k)
            || k.starts_with("TTYPE")
            || k.starts_with("TFORM")
            || k.starts_with("TUNIT")
    };
    rewrite(
        &block.bytes,
        &block.header,
        b,
        &structural,
        &[
            ("XTENSION", "'IMAGE   '".to_string()),
            ("BITPIX", "32".to_string()),
            ("NAXIS", "2".to_string()),
            ("NAXIS1", b.nx.to_string()),
            ("NAXIS2", b.ny.to_string()),
            ("PCOUNT", "0".to_string()),
            ("GCOUNT", "1".to_string()),
        ],
    )
}

/// Keywords of the FITS tiled-image-compression convention (dropped when
/// a compressed HDU is written back as a plain image).
fn is_compression_key(k: &str) -> bool {
    const EXACT: &[&str] = &[
        "ZIMAGE", "ZCMPTYPE", "ZBITPIX", "ZNAXIS", "ZMASKCMP", "ZSIMPLE", "ZTENSION", "ZEXTEND",
        "ZBLOCKED", "ZPCOUNT", "ZGCOUNT", "ZHECKSUM", "ZDATASUM", "ZQUANTIZ", "ZDITHER0",
    ];
    const PREFIX: &[&str] = &["ZNAXIS", "ZTILE", "ZNAME", "ZVAL"];
    EXACT.contains(&k)
        || PREFIX.iter().any(|p| {
            k.strip_prefix(p)
                .is_some_and(|rest| rest.bytes().all(|c| c.is_ascii_digit()))
        })
}

/// Big-endian int32 bytes of `px` (FITS data order).
pub fn i32_be(px: &[i32]) -> Vec<u8> {
    px.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// Concatenate `(header, data)` HDUs, padding each data area to 2880.
pub fn assemble(hdus: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (h, d) in hdus {
        out.extend_from_slice(h);
        out.extend_from_slice(d);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    out
}
