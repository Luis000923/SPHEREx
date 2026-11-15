//! Query-parameter decoding for the API routes (task b5).
//!
//! Every failure is an [`AppError::InvalidParameter`] whose message
//! describes the violated constraint and never echoes the raw input.
//! Coordinates go through the b1 grammar (`parse_coordinates`) and the
//! range check (`validate_query_params`).
//!
//! `/frames` coordinates are then snapped to a preset's exact constants
//! when they match one within [`PRESET_SNAP_ARCSEC`] (task b20): the
//! client caches key on the exact f64 bits of `(ra, dec, fov)`, so a
//! preset position re-typed as rounded decimal degrees (a shared URL,
//! another client) would otherwise miss the baked preset cache
//! entirely. The response echo reports the snapped values.

use std::collections::HashMap;

use axum::extract::Query;
use axum::http::Uri;

use crate::api::validation::{parse_coordinates, validate_query_params, Query as SkyQuery};
use crate::error::AppError;

use super::dto::Mode;

/// Contract defaults for `/frames`.
const DEFAULT_FOV_DEG: f64 = 0.05;
/// Contract default for `max_frames` (also what `WARM_TARGETS` warms).
pub const DEFAULT_MAX_FRAMES: usize = 24;
const MAX_FRAMES_LIMIT: usize = 60;

/// Angular distance (arcseconds) within which a validated `/frames`
/// position snaps to a preset's exact constants (task b20).
///
/// The value must cover the worst 4-decimal-place re-typing of a
/// coordinate (`sqrt(2) * 5e-5 deg` ~ 0.255 arcsec: the observed
/// maximum across the five presets is ~0.19 arcsec) while staying well
/// below the offsets a user can intentionally request (a 0.5-arcsec
/// offset must NOT snap) and far below the preset field sizes
/// (>= 0.05 deg). 0.3 arcsec satisfies both sides with margin.
pub const PRESET_SNAP_ARCSEC: f64 = 0.3;

/// `fov` tolerance (degrees) for the preset snap: the request's field
/// of view must match the preset's within this margin for the snap to
/// apply (a different fov means a different upstream cutout, so a
/// different cache entry is legitimately needed).
const PRESET_SNAP_FOV_TOL_DEG: f64 = 1e-3;

/// Angular distance in degrees between two sky positions (haversine).
/// Exact on the whole sphere: the RA 0/360 wrap is absorbed by the
/// periodic sine, and the RA convergence near the poles falls out of
/// the `cos(dec)` factors, so the snap behaves correctly everywhere.
pub fn angular_dist_deg(ra1_deg: f64, dec1_deg: f64, ra2_deg: f64, dec2_deg: f64) -> f64 {
    let (p1, p2) = (dec1_deg.to_radians(), dec2_deg.to_radians());
    let sdp = ((p1 - p2) / 2.0).sin();
    let sdr = ((ra1_deg - ra2_deg).to_radians() / 2.0).sin();
    let a = sdp.mul_add(sdp, p1.cos() * p2.cos() * sdr * sdr);
    2.0 * a.clamp(0.0, 1.0).sqrt().asin().to_degrees()
}

/// Snap a validated `(ra, dec, fov)` to a preset's exact constants when
/// the position lies within [`PRESET_SNAP_ARCSEC`] of that preset on
/// the sky and `fov` is within [`PRESET_SNAP_FOV_TOL_DEG`] of the
/// preset's. The closest matching preset wins. Everything else —
/// non-preset positions, mismatched fov, non-finite inputs (already
/// rejected by validation) — passes through unchanged.
///
/// This keeps the baked preset cache (and every cache key derived from
/// these exact f64 values) valid for rounded or re-typed coordinates;
/// the snapped values are what upstream lookups and the response echo
/// use.
pub fn snap_to_preset(ra_deg: f64, dec_deg: f64, fov_deg: f64) -> (f64, f64, f64) {
    if !(ra_deg.is_finite() && dec_deg.is_finite() && fov_deg.is_finite()) {
        return (ra_deg, dec_deg, fov_deg);
    }
    let Ok(targets) = super::targets::preset_targets() else {
        return (ra_deg, dec_deg, fov_deg);
    };
    // (distance, ra, dec, fov) of the best matching preset.
    let mut best: Option<(f64, f64, f64, f64)> = None;
    for t in &targets {
        if (t.fov_deg - fov_deg).abs() > PRESET_SNAP_FOV_TOL_DEG {
            continue;
        }
        let d = angular_dist_deg(ra_deg, dec_deg, t.ra_deg, t.dec_deg);
        if best.is_none_or(|(bd, ..)| d < bd) {
            best = Some((d, t.ra_deg, t.dec_deg, t.fov_deg));
        }
    }
    match best {
        Some((d, ra, dec, fov)) if d * 3600.0 <= PRESET_SNAP_ARCSEC => (ra, dec, fov),
        _ => (ra_deg, dec_deg, fov_deg),
    }
}

/// Validated `/api/v1/frames` parameters.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FramesParams {
    /// `ra` resolved to degrees, snapped to a preset when it matches
    /// one (see [`snap_to_preset`]).
    pub ra_deg: f64,
    /// `dec` resolved to degrees, snapped like `ra_deg`.
    pub dec_deg: f64,
    /// `fov`, degrees (0.01-0.5; default 0.05), snapped like `ra_deg`.
    pub fov_deg: f64,
    /// `mode` (default `raw`).
    pub mode: Mode,
    /// Optional `detector` restriction (1-6).
    pub detector: Option<u8>,
    /// `max_frames` (1-60; default 24).
    pub max_frames: usize,
    /// `subtract_zodi` (default false).
    pub subtract_zodi: bool,
}

/// Validated `/api/v1/spectrum` parameters.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpectrumParams {
    /// `ra` resolved to degrees.
    pub ra_deg: f64,
    /// `dec` resolved to degrees.
    pub dec_deg: f64,
    /// `aperture_px` (one of 1, 3, 5; default 3).
    pub aperture_px: u32,
}

/// Parse and validate the `/frames` query string.
pub(crate) fn frames_params(uri: &Uri) -> Result<FramesParams, AppError> {
    let m = query_map(uri)?;
    let (ra, dec) = coordinates(&m)?;
    let fov = opt_f64(&m, "fov", DEFAULT_FOV_DEG)?;
    let sky: SkyQuery = validate_query_params(ra, dec, fov)?;
    let (ra_deg, dec_deg, fov_deg) = snap_to_preset(sky.ra_deg, sky.dec_deg, sky.fov_deg);

    let mode = match m.get("mode") {
        None => Mode::Raw,
        Some(s) => match s.trim().to_ascii_lowercase().as_str() {
            "raw" => Mode::Raw,
            "diff" => Mode::Diff,
            "coadd" => Mode::Coadd,
            "stack" => Mode::Stack,
            _ => {
                return Err(AppError::invalid_parameter(
                    "mode: expected one of raw, diff, coadd, stack",
                ))
            }
        },
    };

    let detector =
        opt_bounded(&m, "detector", None, 1, 6, "expected an integer in [1, 6]")?.map(|d| d as u8);
    let max_frames = opt_bounded(
        &m,
        "max_frames",
        Some(DEFAULT_MAX_FRAMES),
        1,
        MAX_FRAMES_LIMIT,
        "expected an integer in [1, 60]",
    )?
    .unwrap_or(DEFAULT_MAX_FRAMES);
    let subtract_zodi = opt_bool(&m, "subtract_zodi", false)?;

    Ok(FramesParams {
        ra_deg,
        dec_deg,
        fov_deg,
        mode,
        detector,
        max_frames,
        subtract_zodi,
    })
}

/// Parse and validate the `/spectrum` query string.
pub(crate) fn spectrum_params(uri: &Uri) -> Result<SpectrumParams, AppError> {
    let m = query_map(uri)?;
    let (ra, dec) = coordinates(&m)?;
    // `/spectrum` has no `fov` parameter: range-check `ra`/`dec` against
    // the fixed cutout FOV the handler always uses.
    let sky: SkyQuery = validate_query_params(ra, dec, super::spectrum::SPECTRUM_FOV_DEG)?;
    let aperture = opt_bounded(
        &m,
        "aperture_px",
        Some(3),
        1,
        5,
        "expected an integer in {1, 3, 5}",
    )?
    .unwrap_or(3);
    if !matches!(aperture, 1 | 3 | 5) {
        return Err(AppError::invalid_parameter(
            "aperture_px: expected one of 1, 3, 5",
        ));
    }
    Ok(SpectrumParams {
        ra_deg: sky.ra_deg,
        dec_deg: sky.dec_deg,
        aperture_px: aperture as u32,
    })
}

/// URL-decoded query string as a name -> value map (duplicate keys keep
/// the last occurrence).
fn query_map(uri: &Uri) -> Result<HashMap<String, String>, AppError> {
    Query::<HashMap<String, String>>::try_from_uri(uri)
        .map(|Query(m)| m)
        .map_err(|_| AppError::invalid_parameter("malformed query string"))
}

/// Required `ra` + `dec` pair through the b1 grammar.
fn coordinates(m: &HashMap<String, String>) -> Result<(f64, f64), AppError> {
    let ra = required(m, "ra")?;
    let dec = required(m, "dec")?;
    parse_coordinates(ra, dec)
}

fn required<'m>(m: &'m HashMap<String, String>, name: &'static str) -> Result<&'m str, AppError> {
    m.get(name)
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AppError::invalid_parameter(format!("{name}: required parameter is missing"))
        })
}

/// Optional finite `f64`.
fn opt_f64(m: &HashMap<String, String>, name: &'static str, default: f64) -> Result<f64, AppError> {
    match m.get(name) {
        None => Ok(default),
        Some(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| {
                AppError::invalid_parameter(format!("{name}: expected a finite number"))
            }),
    }
}

/// Optional integer in `[min, max]`; `default` applies when absent.
fn opt_bounded(
    m: &HashMap<String, String>,
    name: &'static str,
    default: Option<usize>,
    min: usize,
    max: usize,
    hint: &'static str,
) -> Result<Option<usize>, AppError> {
    let bad = || AppError::invalid_parameter(format!("{name}: {hint}"));
    match m.get(name) {
        None => Ok(default),
        Some(s) => {
            let v: usize = s.trim().parse().map_err(|_| bad())?;
            if (min..=max).contains(&v) {
                Ok(Some(v))
            } else {
                Err(bad())
            }
        }
    }
}

/// Optional boolean (`true/false`, `1/0` accepted, case-insensitive).
fn opt_bool(
    m: &HashMap<String, String>,
    name: &'static str,
    default: bool,
) -> Result<bool, AppError> {
    match m.get(name) {
        None => Ok(default),
        Some(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(AppError::invalid_parameter(format!(
                "{name}: expected a boolean (true/false)"
            ))),
        },
    }
}
