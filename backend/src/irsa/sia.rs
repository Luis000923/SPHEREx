//! SIA2 search: query construction, `Observation` extraction from the
//! VOTable, cross-collection dedup and frame selection.
//!
//! Verified against the live service 2026-11-14 (see
//! `docs/vault/facts.md`): `GET {base}/SIA?COLLECTION=<c>&POS=circle
//! <ra> <dec> <radius_deg>` returns a VOTable whose data table carries
//! `t_min`/`t_max` (`unit="d"`, MJD), `em_min`/`em_max` (`unit="m"`),
//! `energy_bandpassname` and `access_url` columns. There is no detector
//! column: `DETECTOR` is a FITS header keyword read after the cutout
//! download (`crate::fits::FrameMeta::detector`). Until then the only
//! detector hint is `energy_bandpassname` (`"SPHEREx-D3"`, parsed by
//! [`Observation::detector_hint`]) and the only wavelength estimate is
//! the `em_*` band centre ([`Observation::band_center_um`]); both drive
//! [`Observation::select_comparable`], the selection used by the
//! combining modes (`diff`/`coadd`/`stack`).

use std::collections::{BTreeMap, HashSet};

use super::error::IrsaError;
use super::votable::{parse_votable, Votable};

/// sqrt(2)/2: half the diagonal of a unit square. The SIA search circle
/// must cover the requested square cutout, so `radius = fov * FRAC`.
const HALF_DIAGONAL: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// One SIA2 result row: an exposure whose footprint covers the position.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Start of the exposure, MJD (`t_min`).
    pub t_min_mjd: f64,
    /// End of the exposure, MJD (`t_max`).
    pub t_max_mjd: f64,
    /// Lower spectral bound, micrometres (`em_min`, converted).
    pub em_min_um: f64,
    /// Upper spectral bound, micrometres (`em_max`, converted).
    pub em_max_um: f64,
    /// `energy_bandpassname` (e.g. `"SPHEREx-D1"`); empty when absent.
    pub band_name: String,
    /// On-prem IBE access URL of the parent image (cutout base URL).
    pub access_url: String,
    /// Collection this row came from (e.g. `spherex_qr3`).
    pub collection: String,
}

impl Observation {
    /// Stable id: FNV-1a (64-bit) of the access URL, hex. Matches the
    /// API contract ("hash of the access URL") without new dependencies.
    pub fn id(&self) -> String {
        format!("{:016x}", self.id_hash())
    }

    /// Numeric id used inside cache keys (same FNV-1a, as `u64`).
    pub(crate) fn id_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.access_url.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Representative epoch for ordering: mid-exposure when possible.
    fn epoch(&self) -> f64 {
        match (self.t_min_mjd.is_finite(), self.t_max_mjd.is_finite()) {
            (true, true) => f64::midpoint(self.t_min_mjd, self.t_max_mjd),
            (true, false) => self.t_min_mjd,
            _ => self.t_max_mjd,
        }
    }

    /// Detector number (1-6) parsed from `energy_bandpassname`
    /// (`"SPHEREx-D3"` -> 3). SIA2 has no detector column, so this is
    /// the only detector information available **before** the cutout
    /// download (the FITS `DETECTOR` keyword is authoritative but only
    /// readable afterwards). `None` when the band name is absent or
    /// carries no `D<n>` suffix in range.
    pub fn detector_hint(&self) -> Option<u8> {
        let name = self.band_name.trim();
        for (i, c) in name.char_indices().rev() {
            if !matches!(c, 'D' | 'd') {
                continue;
            }
            if let Ok(d) = name[i + c.len_utf8()..].trim().parse::<u8>() {
                if (1..=6).contains(&d) {
                    return Some(d);
                }
            }
        }
        None
    }

    /// Central wavelength (um) of the passband: midpoint of the SIA
    /// spectral bounds `em_min`/`em_max`. A pre-download estimate of the
    /// frame's central wavelength — the WCS-WAVE value at the cutout
    /// centre, read after download, is authoritative. NaN when both
    /// bounds are missing.
    pub fn band_center_um(&self) -> f64 {
        match (self.em_min_um.is_finite(), self.em_max_um.is_finite()) {
            (true, true) => f64::midpoint(self.em_min_um, self.em_max_um),
            (true, false) => self.em_min_um,
            (false, true) => self.em_max_um,
            (false, false) => f64::NAN,
        }
    }

    /// Pick at most `max` observations for the *combining* modes
    /// (`diff`, `coadd`, `stack`).
    ///
    /// Those modes only merge frames of the same detector AND the same
    /// central wavelength within
    /// [`crate::science::DEFAULT_WAVELENGTH_TOLERANCE_UM`]
    /// (`science::group_frames`), so spreading the budget evenly over
    /// the survey — what [`select_frames`] does, and still does for
    /// `raw` — yields mostly singleton groups and starves the mode
    /// (measured live on WISE 0855: 5 usable diff frames out of 24
    /// selected exposures). This selector concentrates the budget on
    /// the largest comparable buckets instead.
    ///
    /// Algorithm (pre-download columns only: detector from
    /// [`detector_hint`](Self::detector_hint), wavelength from
    /// [`band_center_um`](Self::band_center_um); the FITS headers are
    /// only readable after the cutout download):
    ///
    /// 1. Bucket candidates by detector hint; inside each detector,
    ///    cluster rows sorted by band centre into wavelength bins using
    ///    the same anchored rule `group_frames` applies — a new bin
    ///    starts when a centre lies more than
    ///    [`COMPARABLE_WAVELENGTH_BIN_UM`] away from the bin's first
    ///    member. Rows with an unknown band centre each form a
    ///    singleton bin.
    /// 2. Rank buckets by epoch count, then by time baseline.
    /// 3. When the best bucket has fewer than
    ///    [`MIN_COMPARABLE_EPOCHS`] epochs, fall back to the even-spread
    ///    [`select_frames`]: no group is worth concentrating on.
    /// 4. Otherwise fill the budget bucket by bucket, best first:
    ///    take every epoch of a bucket that fits, or `remaining`
    ///    epochs evenly spread over its epoch-sorted members
    ///    (endpoints included, like `select_frames`).
    ///
    /// The result is returned newest first, same as `select_frames`.
    /// When `obs.len() <= max` (or `max == 0`) every observation is
    /// returned: selection cannot improve a set that already fits.
    /// Associated function rather than a free `sia` export because the
    /// `sia` module is private to `crate::irsa`; `Observation` itself
    /// is re-exported, so this stays reachable.
    pub fn select_comparable(obs: &[Observation], max: usize) -> Vec<Observation> {
        if obs.len() <= max || max == 0 {
            return select_frames(obs, max);
        }
        let mut buckets = comparable_buckets(obs);
        buckets.sort_by(|a, b| {
            b.len()
                .cmp(&a.len())
                .then_with(|| bucket_baseline(b).total_cmp(&bucket_baseline(a)))
                .then_with(|| {
                    a.first()
                        .map(|o| o.access_url.as_str())
                        .cmp(&b.first().map(|o| o.access_url.as_str()))
                })
        });
        if buckets.first().map_or(0, Vec::len) < MIN_COMPARABLE_EPOCHS {
            return select_frames(obs, max);
        }
        let mut picked = Vec::with_capacity(max);
        let mut remaining = max;
        for b in &buckets {
            if remaining == 0 {
                break;
            }
            let n = b.len();
            let k = n.min(remaining);
            if k == n {
                picked.extend(b.iter().cloned());
            } else {
                let idx: HashSet<usize> = (0..k).map(|j| (j * (n - 1)) / (k - 1).max(1)).collect();
                picked.extend(
                    b.iter()
                        .enumerate()
                        .filter(|(i, _)| idx.contains(i))
                        .map(|(_, o)| o.clone()),
                );
            }
            remaining -= k;
        }
        picked.sort_by(|a, b| b.epoch().total_cmp(&a.epoch()));
        picked
    }
}

/// Wavelength bin width (um) for [`Observation::select_comparable`]:
/// the same tolerance `science::group_frames` applies when combining
/// frames, so a bucket is exactly the pre-download approximation of a
/// comparable group.
const COMPARABLE_WAVELENGTH_BIN_UM: f64 = crate::science::DEFAULT_WAVELENGTH_TOLERANCE_UM;

/// Fewest epochs that make a bucket worth concentrating the budget on.
/// A diff needs at least two members to yield anything; below three
/// the even-spread selection is the better default.
const MIN_COMPARABLE_EPOCHS: usize = 3;

/// Split `obs` into detector x wavelength buckets (step 1 of
/// [`Observation::select_comparable`]), each sorted by epoch
/// ascending. `None`-detector rows share one "unknown" bucket set.
fn comparable_buckets(obs: &[Observation]) -> Vec<Vec<Observation>> {
    let mut by_det: BTreeMap<Option<u8>, Vec<Observation>> = BTreeMap::new();
    for o in obs {
        by_det.entry(o.detector_hint()).or_default().push(o.clone());
    }
    let mut buckets = Vec::new();
    for (_det, mut rows) in by_det {
        rows.sort_by(|a, b| a.band_center_um().total_cmp(&b.band_center_um()));
        let mut bin: Vec<Observation> = Vec::new();
        let mut anchor = f64::NAN;
        for o in rows {
            let w = o.band_center_um();
            if bin.is_empty() {
                anchor = w;
                bin.push(o);
            } else if (w - anchor).abs() <= COMPARABLE_WAVELENGTH_BIN_UM {
                bin.push(o);
            } else {
                buckets.push(std::mem::take(&mut bin));
                anchor = w;
                bin.push(o);
            }
        }
        if !bin.is_empty() {
            buckets.push(bin);
        }
    }
    for b in &mut buckets {
        b.sort_by(|x, y| x.epoch().total_cmp(&y.epoch()));
    }
    buckets
}

/// Time baseline (days) spanned by a bucket's finite epochs; 0 when
/// fewer than two members carry a finite epoch.
fn bucket_baseline(bucket: &[Observation]) -> f64 {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for o in bucket {
        let e = o.epoch();
        if e.is_finite() {
            lo = lo.min(e);
            hi = hi.max(e);
        }
    }
    if hi >= lo {
        hi - lo
    } else {
        0.0
    }
}

/// SIA search radius (degrees) for a square cutout of side `fov_deg`:
/// the half-diagonal, so every observation covering any part of the
/// requested square is found.
pub fn search_radius_deg(fov_deg: f64) -> f64 {
    fov_deg * HALF_DIAGONAL
}

/// Value of the SIA2 `POS` parameter (a `circle` literal plus the
/// cone). URL-encoded by the HTTP layer before sending.
pub fn pos_parameter(ra_deg: f64, dec_deg: f64, radius_deg: f64) -> String {
    format!("circle {ra_deg} {dec_deg} {radius_deg}")
}

/// Map a VOTable `em_*` value to micrometres.
///
/// The FIELD `unit` attribute drives the conversion (`m`, `um`, `nm`,
/// `angstrom`). IRSA SIA2 emits metres (ObsCore). When the attribute is
/// absent or unrecognised, a sanity heuristic applies: values below
/// 1e-3 cannot be micrometres for SPHEREx (band is ~0.75-5 um), so the
/// value is assumed metres and scaled.
pub fn em_to_um(value: f64, unit: Option<&str>) -> f64 {
    let factor = match unit.map(|u| u.trim().to_ascii_lowercase()) {
        Some(u) if matches!(u.as_str(), "um" | "micron" | "microns" | "µm") => 1.0,
        Some(u) if matches!(u.as_str(), "m" | "meter" | "meters" | "metre" | "metres") => 1e6,
        Some(u) if matches!(u.as_str(), "nm" | "nanometer" | "nanometers") => 1e-3,
        Some(u) if matches!(u.as_str(), "a" | "aa" | "angstrom" | "angstroms") => 1e-4,
        _ => {
            if value.abs() < 1e-3 {
                1e6
            } else {
                1.0
            }
        }
    };
    value * factor
}

/// Map a VOTable `t_*` value to MJD days. `unit="d"` (ObsCore) passes
/// through; seconds are scaled. Unknown units are assumed days.
pub fn time_to_mjd(value: f64, unit: Option<&str>) -> f64 {
    match unit.map(|u| u.trim().to_ascii_lowercase()) {
        Some(u) if matches!(u.as_str(), "s" | "sec" | "secs" | "second" | "seconds") => {
            value / 86400.0
        }
        _ => value,
    }
}

/// Parse an optional double cell; non-numeric and non-finite values
/// (VOTable `NaN` sentinels) become `None`.
fn parse_f64(cell: Option<&str>) -> Option<f64> {
    cell.and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Extract observations from all SIA VOTables of a response for
/// `collection`. Rows without an `access_url` are unusable for cutouts
/// and skipped; missing numerics become NaN.
pub fn observations_from_votable(
    xml: &str,
    collection: &str,
) -> Result<Vec<Observation>, IrsaError> {
    let tables = parse_votable(xml)?;
    // The data table is the first one with an access_url column (IRSA
    // prepends a service-metadata RESOURCE without data tables).
    let table = tables
        .iter()
        .find(|t| t.column("access_url").is_some())
        .or_else(|| tables.iter().find(|t| !t.fields.is_empty()));
    let Some(t) = table else {
        return Err(IrsaError::Parse("no data table in VOTable".into()));
    };
    Ok(rows_to_observations(t, collection))
}

/// Row-by-row conversion; pure and testable.
fn rows_to_observations(t: &Votable, collection: &str) -> Vec<Observation> {
    let c_url = t.column("access_url");
    let c_tmin = t.column("t_min");
    let c_tmax = t.column("t_max");
    let c_emin = t.column("em_min");
    let c_emax = t.column("em_max");
    let c_band = t.column("energy_bandpassname");
    let u_emin = t.unit("em_min");
    let u_emax = t.unit("em_max");
    let u_tmin = t.unit("t_min");
    let u_tmax = t.unit("t_max");

    let mut out = Vec::with_capacity(t.rows.len());
    for row in &t.rows {
        let Some(url) = t.cell(row, c_url) else {
            continue;
        };
        out.push(Observation {
            t_min_mjd: parse_f64(t.cell(row, c_tmin))
                .map(|v| time_to_mjd(v, u_tmin))
                .unwrap_or(f64::NAN),
            t_max_mjd: parse_f64(t.cell(row, c_tmax))
                .map(|v| time_to_mjd(v, u_tmax))
                .unwrap_or(f64::NAN),
            em_min_um: parse_f64(t.cell(row, c_emin))
                .map(|v| em_to_um(v, u_emin))
                .unwrap_or(f64::NAN),
            em_max_um: parse_f64(t.cell(row, c_emax))
                .map(|v| em_to_um(v, u_emax))
                .unwrap_or(f64::NAN),
            band_name: t.cell(row, c_band).unwrap_or_default().to_string(),
            access_url: url.to_string(),
            collection: collection.to_string(),
        });
    }
    out
}

/// Drop duplicates across collections keyed by `access_url`; the first
/// occurrence (earliest collection in config order) wins.
pub fn dedup_by_url(obs: Vec<Observation>) -> Vec<Observation> {
    let mut seen = HashSet::with_capacity(obs.len());
    obs.into_iter()
        .filter(|o| seen.insert(o.access_url.clone()))
        .collect()
}

/// Pick at most `max` observations to fetch.
///
/// Observations are sorted by epoch and, when more than `max` exist,
/// `max` evenly spaced indices are kept so the selection is spread over
/// the full time range rather than clustered at one end. The result is
/// returned newest first (descending epoch).
pub fn select_frames(obs: &[Observation], max: usize) -> Vec<Observation> {
    let mut sorted = obs.to_vec();
    sorted.sort_by(|a, b| a.epoch().total_cmp(&b.epoch()));
    let n = sorted.len();
    if n <= max || max == 0 {
        sorted.reverse();
        return sorted;
    }
    let mut picked: Vec<Observation> = (0..max)
        .map(|k| (k * (n - 1)) / (max - 1).max(1))
        .collect::<HashSet<usize>>()
        .into_iter()
        .map(|i| sorted[i].clone())
        .collect();
    picked.sort_by(|a, b| b.epoch().total_cmp(&a.epoch()));
    picked
}

/// Value of the IBE cutout `size` parameter for a square field of view.
///
/// Units are defined by https://irsa.ipac.caltech.edu/ibe/cutouts.html:
/// `size` accepts pixel units (`px`, `pix`, `pixels`) or angular units
/// (`arcsec`, `arcmin`, `deg`, `rad`), defaults to **degrees**, and a
/// single value is used as both full-width and full-height (square).
/// We always emit an explicit `deg` suffix so the meaning is pinned
/// even if server defaults ever change.
pub fn fov_to_cutout_size(fov_deg: f64) -> String {
    format!("{fov_deg}deg")
}

/// Full cutout URL for an observation:
/// `{access_url}?center=<ra>,<dec>&size=<fov>deg` (`center` also
/// defaults to degrees, `RA,Dec` J2000). If the access URL already has
/// a query string the parameters are appended with `&`.
///
/// `gzip` is not requested: per the IBE docs SPHEREx cutouts are never
/// gzipped, and `reqwest` transparently decodes content-encoding anyway.
pub fn cutout_url(access_url: &str, ra_deg: f64, dec_deg: f64, fov_deg: f64) -> String {
    let sep = if access_url.contains('?') { '&' } else { '?' };
    format!(
        "{access_url}{sep}center={ra_deg},{dec_deg}&size={}",
        fov_to_cutout_size(fov_deg)
    )
}
