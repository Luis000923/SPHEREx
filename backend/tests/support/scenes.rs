//! Realistic synthetic SPHEREx sky scenes for the task-b8 scientific
//! validation battery (`tests/science_validation_tests.rs`).
//!
//! A [`Scene`] carries a TAN (+ optional SIP) celestial WCS at the
//! SPHEREx-like ~6.15 arcsec/pixel scale, `DETECTOR`/`MJD-*` metadata, a
//! `WCS-WAVE` lookup whose wavelength varies linearly across the array
//! (like the real LVF spectral images), a FLAGS legend
//! (`MSKNnnnn`/`MSKDnnnn`), VARIANCE and a ZODI model plane.
//! [`Scene::to_bytes`] serialises it to a multi-extension FITS in memory
//! via the shared `tests/fixtures` writers; [`Scene::to_frame`] runs it
//! through `fits::parse_frame` so tests exercise the real parser.
//!
//! [`IndepWcs`] is a second, from-scratch implementation of the TAN+SIP
//! transform (classic Calabretta & Greisen gnomonic formulae plus
//! fixed-point SIP inversion). Tests derive every ground-truth position
//! from it, so the pipeline is checked against independent math rather
//! than against its own code.
//!
//! All randomness goes through the seeded [`Rng`] (xorshift64* +
//! Box-Muller): every test is deterministic and offline.

#![allow(dead_code)]

use crate::fixtures::{
    card_flt, card_int, card_str, f32_data, i32_data, image_hdu, primary_hdu, wcs_wave_hdu,
};
use spherex_backend::fits::{parse_frame, Frame};

/// SPHEREx pixel scale, degrees per pixel (~6.15 arcsec).
pub const PIXEL_SCALE_DEG: f64 = 6.15 / 3600.0;

const DEG: f64 = std::f64::consts::PI / 180.0;

// ---------------------------------------------------------------------
// Deterministic RNG (xorshift64* + Box-Muller)
// ---------------------------------------------------------------------

/// Small deterministic PRNG; same seed -> same sequence, every platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform double in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
    }

    /// Standard normal deviate (Box-Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = (1.0 - self.unit()).max(f64::MIN_POSITIVE);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

// ---------------------------------------------------------------------
// Independent TAN + SIP world-coordinate model
// ---------------------------------------------------------------------

/// Independent reimplementation of the celestial WCS used to compute
/// ground truth for the tests. Convention matches the pipeline: 1-based
/// FITS pixel coordinates, degrees.
#[derive(Debug, Clone)]
pub struct IndepWcs {
    /// Reference pixel `[x, y]` (CRPIX).
    pub crpix: [f64; 2],
    /// `[ra_deg, dec_deg]` at `crpix` (CRVAL).
    pub crval: [f64; 2],
    /// Degrees-per-pixel matrix `[[cd11, cd12], [cd21, cd22]]`.
    pub cd: [[f64; 2]; 2],
    /// Forward SIP terms correcting `u`: `(p, q, coeff)` of `u^p v^q`.
    pub sip_a: SipTerms,
    /// Forward SIP terms correcting `v`.
    pub sip_b: SipTerms,
}

/// Evaluate `sum c_pq u^p v^q`.
fn poly(terms: &[(u32, u32, f64)], u: f64, v: f64) -> f64 {
    terms
        .iter()
        .map(|&(p, q, c)| c * u.powi(p as i32) * v.powi(q as i32))
        .sum()
}

impl IndepWcs {
    /// 1-based pixel `(x, y)` -> `(ra_deg, dec_deg)`: forward SIP, CD
    /// matrix, classic gnomonic projection.
    pub fn pixel_to_sky(&self, x: f64, y: f64) -> (f64, f64) {
        let u0 = x - self.crpix[0];
        let v0 = y - self.crpix[1];
        // Both corrections are evaluated on the original offsets.
        let u = u0 + poly(&self.sip_a, u0, v0);
        let v = v0 + poly(&self.sip_b, u0, v0);
        let xi = (self.cd[0][0] * u + self.cd[0][1] * v) * DEG;
        let eta = (self.cd[1][0] * u + self.cd[1][1] * v) * DEG;
        let ra0 = self.crval[0] * DEG;
        let dec0 = self.crval[1] * DEG;
        let ra = ra0 + xi.atan2(dec0.cos() - eta * dec0.sin());
        let dec = ((dec0.sin() + eta * dec0.cos()) / (1.0 + xi * xi + eta * eta).sqrt()).asin();
        (ra.to_degrees().rem_euclid(360.0), dec.to_degrees())
    }

    /// `(ra_deg, dec_deg)` -> 1-based pixel `(x, y)`: gnomonic inverse,
    /// CD inverse, fixed-point SIP inversion.
    pub fn sky_to_pixel(&self, ra_deg: f64, dec_deg: f64) -> (f64, f64) {
        let ra = ra_deg * DEG;
        let dec = dec_deg * DEG;
        let ra0 = self.crval[0] * DEG;
        let dec0 = self.crval[1] * DEG;
        let dra = ra - ra0;
        let denom = dec0.sin() * dec.sin() + dec0.cos() * dec.cos() * dra.cos();
        let xi = dec.cos() * dra.sin() / denom / DEG;
        let eta = (dec0.cos() * dec.sin() - dec0.sin() * dec.cos() * dra.cos()) / denom / DEG;
        let det = self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0];
        let up = (self.cd[1][1] * xi - self.cd[0][1] * eta) / det;
        let vp = (-self.cd[1][0] * xi + self.cd[0][0] * eta) / det;
        // Undo SIP: u = u' - f(u, v) by fixed-point iteration.
        let (mut u, mut v) = (up, vp);
        for _ in 0..80 {
            let nu = up - poly(&self.sip_a, u, v);
            let nv = vp - poly(&self.sip_b, u, v);
            if (nu - u).abs() < 1e-12 && (nv - v).abs() < 1e-12 {
                u = nu;
                v = nv;
                break;
            }
            u = nu;
            v = nv;
        }
        (u + self.crpix[0], v + self.crpix[1])
    }
}

/// Forward SIP polynomial terms: `(p, q, coeff)` of `u^p v^q`.
pub type SipTerms = Vec<(u32, u32, f64)>;

/// Forward SIP coefficient sets enabled by `Scene::sip(true)`: small
/// order-3 distortion, realistic for a wide-field imager (~0.1-0.2 px at
/// the array edges of the test scenes).
pub fn sip_terms() -> (SipTerms, SipTerms) {
    (
        vec![
            (2, 0, 6.0e-5),
            (1, 1, -3.0e-5),
            (0, 2, 4.0e-5),
            (3, 0, 8.0e-8),
            (2, 1, 5.0e-8),
            (0, 3, -6.0e-8),
        ],
        vec![
            (2, 0, -4.0e-5),
            (1, 1, 5.0e-5),
            (0, 2, -6.0e-5),
            (3, 0, -5.0e-8),
            (1, 2, 6.0e-8),
            (0, 3, 7.0e-8),
        ],
    )
}

/// CD matrix at [`PIXEL_SCALE_DEG`] for a detector rotated `theta_deg`
/// on the sky. `mirror = false` gives the normal north-up/east-left
/// handedness (det < 0); `mirror = true` flips the parity (det > 0).
pub fn cd_matrix(theta_deg: f64, mirror: bool) -> [[f64; 2]; 2] {
    let s = PIXEL_SCALE_DEG;
    let b = if mirror {
        [[s, 0.0], [0.0, s]]
    } else {
        [[-s, 0.0], [0.0, s]]
    };
    let (sn, c) = (theta_deg * DEG).sin_cos();
    [
        [c * b[0][0] - sn * b[1][0], c * b[0][1] - sn * b[1][1]],
        [sn * b[0][0] + c * b[1][0], sn * b[0][1] + c * b[1][1]],
    ]
}

/// FLAGS legend entries `(bit, MSKN name, MSKD description)` written to
/// every scene's FLAGS header. Bit meanings live only here; tests look
/// them up by name via [`flag_bit`] / `mask_from_names`, never by number.
pub fn flag_defs() -> Vec<(i64, &'static str, &'static str)> {
    vec![
        (0, "TRANSIENT", "Transient source in the pixel"),
        (1, "SATURATED", "Pixel saturated"),
        (4, "PERSIST", "Persistence from a previous exposure"),
        (10, "HOT", "Hot pixel"),
        (21, "SOURCE", "Pixel mapped to a known source"),
        (28, "HALO", "Bright source halo"),
    ]
}

/// Bit number of a flag `name` from the scene legend; panics on unknown.
pub fn flag_bit(name: &str) -> i64 {
    flag_defs()
        .iter()
        .find(|(_, n, _)| *n == name)
        .unwrap_or_else(|| panic!("unknown scene flag {name}"))
        .0
}

// ---------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------

/// A synthetic SPHEREx-like observation plus the truth needed to verify
/// pipeline outputs. Pixel arrays are row-major, `index = y * nx + x`
/// with 0-based `x`, `y` (matching `Array2[y][x]` after parsing).
#[derive(Debug, Clone)]
pub struct Scene {
    /// Columns (FITS NAXIS1).
    pub nx: usize,
    /// Rows (FITS NAXIS2).
    pub ny: usize,
    /// `DETECTOR` keyword (1-6); `None` omits the card.
    pub detector: Option<i64>,
    /// `MJD-AVG` (MJD-OBS/BEG/END derive from it).
    pub mjd_avg: f64,
    /// Truth WCS actually written to the header (SIP terms empty when
    /// `sip_on` is false).
    pub wcs: IndepWcs,
    /// True when A_/B_ SIP cards are written and `wcs` carries the terms.
    pub sip_on: bool,
    /// Detector rotation on the sky (degrees).
    pub theta_deg: f64,
    /// Mirrored readout (CD determinant > 0) when true.
    pub mirrored: bool,
    /// `lambda(x, y) = wave_center_um + dldx*(x - xc) + dldy*(y - yc)`
    /// in um, with `(xc, yc)` the 1-based array centre.
    pub wave_center_um: f64,
    /// Wavelength gradient along x, um/px.
    pub dldx: f64,
    /// Wavelength gradient along y, um/px.
    pub dldy: f64,
    /// `bandwidth = wavelength * band_frac` in the WCS-WAVE table.
    pub band_frac: f64,
    /// Observed surface brightness, MJy/sr (`sky + zodi` once
    /// `zodi_plane` has run, like real SPHEREx IMAGE).
    pub image: Vec<f32>,
    /// Variance, (MJy/sr)^2.
    pub variance: Vec<f32>,
    /// FLAGS bitmask per pixel.
    pub flags: Vec<i32>,
    /// ZODI model, MJy/sr.
    pub zodi: Vec<f32>,
}

impl Scene {
    /// `nx * ny` scene with defaults: detector 1, MJD 61000, no rotation,
    /// no mirror, no SIP, `crval = (150, 2)`, `crpix` = array centre,
    /// wave plane 1.10 um flat-ish, zeroed pixels.
    pub fn new(nx: usize, ny: usize) -> Self {
        Self {
            nx,
            ny,
            detector: Some(1),
            mjd_avg: 61000.0,
            wcs: IndepWcs {
                crpix: [(nx as f64 + 1.0) / 2.0, (ny as f64 + 1.0) / 2.0],
                crval: [150.0, 2.0],
                cd: cd_matrix(0.0, false),
                sip_a: Vec::new(),
                sip_b: Vec::new(),
            },
            sip_on: false,
            theta_deg: 0.0,
            mirrored: false,
            wave_center_um: 1.10,
            dldx: 0.002,
            dldy: -0.001,
            band_frac: 1.0 / 41.0,
            image: vec![0.0; nx * ny],
            variance: vec![1.0; nx * ny],
            flags: vec![0; nx * ny],
            zodi: vec![0.0; nx * ny],
        }
    }

    fn rebuild_cd(&mut self) {
        self.wcs.cd = cd_matrix(self.theta_deg, self.mirrored);
    }

    /// Rotate the detector `deg` on the sky.
    pub fn rotation(mut self, deg: f64) -> Self {
        self.theta_deg = deg;
        self.rebuild_cd();
        self
    }

    /// Mirror the readout (CD determinant sign flip).
    pub fn mirror(mut self, on: bool) -> Self {
        self.mirrored = on;
        self.rebuild_cd();
        self
    }

    /// Enable/disable SIP distortion (writes A_/B_ cards and applies the
    /// terms to the truth WCS).
    pub fn sip(mut self, on: bool) -> Self {
        let (a, b) = if on {
            sip_terms()
        } else {
            (Vec::new(), Vec::new())
        };
        self.wcs.sip_a = a;
        self.wcs.sip_b = b;
        self.sip_on = on;
        self
    }

    /// Pointing centre (CRVAL) in degrees.
    pub fn pointing(mut self, ra_deg: f64, dec_deg: f64) -> Self {
        self.wcs.crval = [ra_deg, dec_deg];
        self
    }

    /// Offset the pointing by `(dx, dy)` tangent-plane pixels of the
    /// *linear* (SIP-free) WCS: the telescope is repointed so that the
    /// sky at `crpix + (dx, dy)` becomes the new CRVAL.
    pub fn pointing_offset_px(mut self, dx: f64, dy: f64) -> Self {
        let lin = IndepWcs {
            sip_a: Vec::new(),
            sip_b: Vec::new(),
            ..self.wcs.clone()
        };
        let (ra, dec) = lin.pixel_to_sky(self.wcs.crpix[0] + dx, self.wcs.crpix[1] + dy);
        self.wcs.crval = [ra, dec];
        self
    }

    /// `DETECTOR` keyword value.
    pub fn detector(mut self, d: i64) -> Self {
        self.detector = Some(d);
        self
    }

    /// Omit the `DETECTOR` keyword entirely.
    pub fn no_detector(mut self) -> Self {
        self.detector = None;
        self
    }

    /// `MJD-AVG` (MJD-BEG/OBS/END derive around it).
    pub fn mjd(mut self, m: f64) -> Self {
        self.mjd_avg = m;
        self
    }

    /// Wavelength plane: central value plus gradients, um/px.
    pub fn wave(mut self, center_um: f64, dldx: f64, dldy: f64) -> Self {
        self.wave_center_um = center_um;
        self.dldx = dldx;
        self.dldy = dldy;
        self
    }

    /// Fill IMAGE with `v` (before zodi/noise).
    pub fn fill(&mut self, v: f32) {
        self.image.fill(v);
    }

    /// Fill VARIANCE with `v`.
    pub fn set_variance(&mut self, v: f32) {
        self.variance.fill(v);
    }

    /// ZODI model plane `z(x, y) = z0 + zx*(x - xc) + zy*(y - yc)`;
    /// also added to IMAGE so `IMAGE = sky + zodi` as in real deliveries.
    pub fn zodi_plane(&mut self, z0: f32, zx: f32, zy: f32) {
        let xc = (self.nx as f64 + 1.0) / 2.0;
        let yc = (self.ny as f64 + 1.0) / 2.0;
        for y in 0..self.ny {
            for x in 0..self.nx {
                let z = z0 + zx * (x as f64 + 1.0 - xc) as f32 + zy * (y as f64 + 1.0 - yc) as f32;
                self.zodi[y * self.nx + x] = z;
                self.image[y * self.nx + x] += z;
            }
        }
    }

    /// Add a circular Gaussian PSF centred on 1-based pixel `(x, y)`
    /// (fractional allowed), `amp` peak MJy/sr.
    pub fn gaussian_at_pix(&mut self, x: f64, y: f64, amp: f32, fwhm_px: f32) {
        let sigma = f64::from(fwhm_px) / 2.354_820_045;
        let r = (4.0 * sigma).ceil().max(2.0) as i64;
        let (cx, cy) = (x.round() as i64, y.round() as i64);
        for iy in (cy - r)..=(cy + r) {
            for ix in (cx - r)..=(cx + r) {
                if !(1..=self.nx as i64).contains(&ix) || !(1..=self.ny as i64).contains(&iy) {
                    continue;
                }
                let dx = ix as f64 - x;
                let dy = iy as f64 - y;
                let g = f64::from(amp) * (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
                self.image[(iy - 1) as usize * self.nx + (ix - 1) as usize] += g as f32;
            }
        }
    }

    /// Gaussian PSF at a sky position; returns the 1-based pixel where
    /// the truth WCS placed it.
    pub fn gaussian_at_sky(
        &mut self,
        ra_deg: f64,
        dec_deg: f64,
        amp: f32,
        fwhm_px: f32,
    ) -> (f64, f64) {
        let (x, y) = self.wcs.sky_to_pixel(ra_deg, dec_deg);
        self.gaussian_at_pix(x, y, amp, fwhm_px);
        (x, y)
    }

    /// Add iid Gaussian noise `N(0, sigma)` to IMAGE.
    pub fn add_noise(&mut self, rng: &mut Rng, sigma: f32) {
        for v in &mut self.image {
            *v += (rng.gauss() * f64::from(sigma)) as f32;
        }
    }

    /// OR flag `bit` into 0-based pixel `(x, y)`.
    pub fn set_flag(&mut self, x: usize, y: usize, bit: i64) {
        self.flags[y * self.nx + x] |= 1i32 << bit;
    }

    /// Truth wavelength (um) at 1-based pixel `(x, y)` — the plane the
    /// WCS-WAVE table samples.
    pub fn wave_at(&self, x: f64, y: f64) -> f64 {
        let xc = (self.nx as f64 + 1.0) / 2.0;
        let yc = (self.ny as f64 + 1.0) / 2.0;
        self.wave_center_um + self.dldx * (x - xc) + self.dldy * (y - yc)
    }

    /// Serialise to an in-memory multi-extension FITS cutout:
    /// primary + IMAGE (TAN[+SIP] WCS, metadata) + FLAGS (legend) +
    /// VARIANCE + ZODI + WCS-WAVE (3x3 control grid).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut img = vec![
            card_str(
                "CTYPE1",
                if self.sip_on {
                    "RA---TAN-SIP"
                } else {
                    "RA---TAN"
                },
            ),
            card_str(
                "CTYPE2",
                if self.sip_on {
                    "DEC--TAN-SIP"
                } else {
                    "DEC--TAN"
                },
            ),
            card_flt("CRPIX1", self.wcs.crpix[0]),
            card_flt("CRPIX2", self.wcs.crpix[1]),
            card_flt("CRVAL1", self.wcs.crval[0]),
            card_flt("CRVAL2", self.wcs.crval[1]),
            card_flt("CD1_1", self.wcs.cd[0][0]),
            card_flt("CD1_2", self.wcs.cd[0][1]),
            card_flt("CD2_1", self.wcs.cd[1][0]),
            card_flt("CD2_2", self.wcs.cd[1][1]),
            // WCS-WAVE pixel -> table-coordinate transform (identity) and
            // the VALUES pair indices (wavelength, bandwidth).
            card_flt("CRPIX1W", 0.0),
            card_flt("CRPIX2W", 0.0),
            card_flt("CRVAL1W", 0.0),
            card_flt("CRVAL2W", 0.0),
            card_int("CDELT1W", 1),
            card_int("CDELT2W", 1),
            card_int("PV1_3W", 1),
            card_int("PV2_3W", 2),
            card_str("BUNIT", "MJy / sr"),
            card_str("DATE-OBS", "2026-05-20T21:38:04.643"),
            card_flt("MJD-BEG", self.mjd_avg - 0.0007),
            card_flt("MJD-OBS", self.mjd_avg - 0.00035),
            card_flt("MJD-AVG", self.mjd_avg),
            card_flt("MJD-END", self.mjd_avg + 0.0007),
            card_flt("XPOSURE", 113.58),
            card_flt("TELAPSE", 118.19),
        ];
        if let Some(d) = self.detector {
            img.push(card_int("DETECTOR", d));
        }
        if self.sip_on {
            img.push(card_int("A_ORDER", 3));
            for &(p, q, c) in &self.wcs.sip_a {
                img.push(card_flt(&format!("A_{p}_{q}"), c));
            }
            img.push(card_int("B_ORDER", 3));
            for &(p, q, c) in &self.wcs.sip_b {
                img.push(card_flt(&format!("B_{p}_{q}"), c));
            }
        }

        let mut f = primary_hdu();
        f.extend(image_hdu(
            "IMAGE",
            -32,
            self.nx,
            self.ny,
            &f32_data(&self.image),
            &img,
        ));
        let mut legend = Vec::new();
        for (bit, name, desc) in flag_defs() {
            legend.push(card_str(&format!("MSKN{bit:04}"), name));
            legend.push(card_str(&format!("MSKD{bit:04}"), desc));
        }
        f.extend(image_hdu(
            "FLAGS",
            32,
            self.nx,
            self.ny,
            &i32_data(&self.flags),
            &legend,
        ));
        f.extend(image_hdu(
            "VARIANCE",
            -32,
            self.nx,
            self.ny,
            &f32_data(&self.variance),
            &[],
        ));
        f.extend(image_hdu(
            "ZODI",
            -32,
            self.nx,
            self.ny,
            &f32_data(&self.zodi),
            &[],
        ));
        // WCS-WAVE lookup: 3x3 control grid over the array, VALUES =
        // (wavelength, bandwidth) pairs, x fastest — the real layout.
        let xs: Vec<i32> = vec![1, ((self.nx / 2).max(2)) as i32, self.nx as i32];
        let ys: Vec<i32> = vec![1, ((self.ny / 2).max(2)) as i32, self.ny as i32];
        let mut values = Vec::with_capacity(xs.len() * ys.len() * 2);
        for &yy in &ys {
            for &xx in &xs {
                let w = self.wave_at(f64::from(xx), f64::from(yy)) as f32;
                values.push(w);
                values.push((f64::from(w) * self.band_frac) as f32);
            }
        }
        f.extend(wcs_wave_hdu(&xs, &ys, &values, &[]));
        f
    }

    /// Serialise and run through `fits::parse_frame`.
    pub fn to_frame(&self) -> Frame {
        parse_frame(&self.to_bytes()).expect("synthetic scene must parse")
    }
}
