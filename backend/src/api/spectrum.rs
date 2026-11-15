//! `GET /api/v1/spectrum` (task b5; latency pass b14; cache reuse b18).
//!
//! Validates `ra`/`dec`/`aperture_px`, fetches the cutouts covering the
//! point on the smallest allowed FOV under the request deadline, then
//! calls [`extract_spectrum`]. An empty extraction is a 404
//! (`not_found`), exactly as the contract specifies.
//!
//! Latency behaviour matches `/frames`: the SIA search and the cutout
//! downloads share one request deadline, [`fetch_cutouts`] answers at
//! the soft deadline with whatever has decoded and keeps the rest
//! running in the background so the next identical request is complete
//! and fast, and the handler never errors while at least one point
//! exists. Upstream positions are quantised to [`SPECTRUM_POS_CELL_DEG`]
//! so clicks a few arcseconds apart share the search/cutout cache keys
//! instead of each paying a full cold fetch.
//!
//! Cache reuse (b18): before paying any upstream traffic, the handler
//! probes the source's coverage cache. A cached SIA cone containing the
//! click is a superset of the observations covering it, so the search
//! is skipped entirely; each selected observation is then served from
//! the smallest cached cutout fully containing the click plus the
//! aperture margin (the frame is used as is — aperture extraction works
//! on any frame via WCS). Only observations with no covering cached
//! cutout fall back to the download path below. A click inside a field
//! `/frames` already downloaded — e.g. the baked presets — therefore
//! answers with zero upstream traffic.

use axum::{extract::State, http::Uri, Json};

use crate::api::validation::{DEC_MAX_DEG, DEC_MIN_DEG};
use crate::error::AppError;
use crate::fits::Frame;
use crate::irsa::{select_frames, CutoutFrame};
use crate::science::extract_spectrum;

use super::dto::{SpectrumPointDto, SpectrumResponseDto};
use super::fetch::{fetch_cutouts, search_until, SearchOutcome};
use super::params;
use super::{AppState, FrameSource};
use tokio::time::Instant;

/// Cutout side for spectrum queries: the smallest allowed FOV (0.01 deg
/// ~ 36") still covers the largest aperture (5 px * ~6.15"/px ~ 31").
pub(crate) const SPECTRUM_FOV_DEG: f64 = 0.01;

/// Spectrum fluxes are zodi-subtracted: the zodiacal model would
/// otherwise dominate the aperture mean and drown the source.
const SPECTRUM_SUBTRACT_ZODI: bool = true;

/// Most cutouts fetched for one spectrum click. Each cutout is a full
/// upstream file read on IRSA; 60 per click (the frames cap) overran the
/// deadline under load. 24 epochs still sample 0.75-5 um across all six
/// detectors.
pub(crate) const SPECTRUM_MAX_FRAMES: usize = 24;

/// Snap step for the upstream position (SIA centre and cutout centre),
/// degrees.
///
/// The contract gives `/spectrum` no FOV and the frontend sends the
/// clicked pixel's exact sky coordinates, so every click used to be a
/// different cache key: a fresh SIA search plus a full set of cutout
/// downloads even a few arcseconds away from a warm position. Snapping
/// the upstream position to a grid makes clicks inside one cell share
/// the search result and the decoded cutouts (the client's in-memory
/// and disk caches and its `try_get_with` coalescing all key on the
/// exact tuple).
///
/// The cell stays well under the cutout half-side
/// (`SPECTRUM_FOV_DEG` / 2 = 18"), so a click is at most cell/sqrt(2)
/// ~ 12.7" from the cutout centre and always inside it; the search cone
/// (half the cutout's diagonal) still covers the whole cell. The
/// aperture extraction and the response keep the exact requested
/// coordinates.
const SPECTRUM_POS_CELL_DEG: f64 = 0.005;

/// Upper bound on the native pixel scale used to turn the aperture
/// into a sky margin: ~9"/px comfortably covers the SPHEREx ~6.15"/px
/// detectors. The client converts the margin back to pixels with each
/// frame's own CD scale, so an over-estimate here only makes the
/// coverage check slightly stricter — it can never admit a cutout
/// whose aperture would fall off the array.
const SPECTRUM_PX_SCALE_DEG: f64 = 9.0 / 3600.0;

/// Sky margin (degrees) the click needs inside a cached cutout so the
/// `aperture_px` square fully fits: half the aperture plus half a pixel
/// of sub-pixel slack, at the conservative scale bound.
fn aperture_margin_deg(aperture_px: u32) -> f64 {
    (f64::from(aperture_px / 2) + 0.5) * SPECTRUM_PX_SCALE_DEG
}

/// Position the upstream queries run at: `(ra_deg, dec_deg)` snapped to
/// the nearest grid centre. `ra` wraps back into `[0, 360)` (a click at
/// 359.99x can snap onto 360.0) and `dec` stays inside `[-90, 90]`.
fn upstream_pos(ra_deg: f64, dec_deg: f64) -> (f64, f64) {
    let snap = |v: f64| (v / SPECTRUM_POS_CELL_DEG).round() * SPECTRUM_POS_CELL_DEG;
    (
        snap(ra_deg).rem_euclid(360.0),
        snap(dec_deg).clamp(DEC_MIN_DEG, DEC_MAX_DEG),
    )
}

/// `GET /api/v1/spectrum` handler.
pub(crate) async fn get_spectrum<S: FrameSource>(
    State(state): State<AppState<S>>,
    uri: Uri,
) -> Result<Json<SpectrumResponseDto>, AppError> {
    let p = params::spectrum_params(&uri)?;

    // One deadline for the whole request: SIA search + cutouts.
    let start = Instant::now();
    let until = start + state.request_deadline;
    let soft = start + state.soft_deadline.min(state.request_deadline);
    // Quantised upstream position: identical and nearby clicks share the
    // client's search and cutout caches. The exact requested position is
    // still what the aperture extraction and the response report.
    let (ura, udec) = upstream_pos(p.ra_deg, p.dec_deg);
    // Observation list: a cached SIA cone containing the click is a
    // superset of the observations covering it, so when one exists (the
    // /frames cones of the baked presets, or an earlier click) the
    // upstream search is skipped entirely.
    let observations = match state.source.cached_observations(p.ra_deg, p.dec_deg).await {
        Some(list) => list,
        None => match search_until(&state.source, ura, udec, SPECTRUM_FOV_DEG, until).await {
            SearchOutcome::Done(res) => res.map_err(AppError::from)?,
            SearchOutcome::DeadlineHit => return Err(AppError::UpstreamUnavailable),
        },
    };
    // Same selection `/frames` raw makes, so the observations match the
    // ones whose cutouts are already cached.
    let selected = select_frames(&observations, state.max_frames_cap.min(SPECTRUM_MAX_FRAMES));

    // Reuse the cutouts /frames (or an earlier click) already cached: a
    // cached frame whose box fully contains the click plus the aperture
    // margin is used as is — extraction works on any frame via WCS.
    // Only observations with no covering cutout go through the fetch
    // path; a fully covered click performs zero upstream calls.
    let margin = aperture_margin_deg(p.aperture_px);
    let mut frames = Vec::with_capacity(selected.len());
    let mut missing = Vec::new();
    for obs in selected {
        match state
            .source
            .cached_cutout(obs.clone(), p.ra_deg, p.dec_deg, margin)
            .await
        {
            Some(frame) => frames.push(CutoutFrame {
                observation: obs,
                frame,
            }),
            None => missing.push(obs),
        }
    }
    let fetched = fetch_cutouts(
        &state.source,
        missing,
        ura,
        udec,
        SPECTRUM_FOV_DEG,
        soft,
        until,
    )
    .await;
    frames.extend(fetched.frames);
    if frames.is_empty() {
        return Err(if fetched.deadline_hit {
            AppError::UpstreamUnavailable
        } else {
            fetched
                .first_error
                .map_or(AppError::NotFound, AppError::from)
        });
    }

    let refs: Vec<&Frame> = frames.iter().map(|c| c.frame.as_ref()).collect();
    let points = extract_spectrum(
        &refs,
        p.ra_deg,
        p.dec_deg,
        p.aperture_px,
        SPECTRUM_SUBTRACT_ZODI,
    )
    .map_err(AppError::from)?;
    if points.is_empty() {
        return Err(AppError::NotFound);
    }

    Ok(Json(SpectrumResponseDto {
        ra_deg: p.ra_deg,
        dec_deg: p.dec_deg,
        unit: "MJy/sr".to_string(),
        points: points
            .iter()
            .map(|pt| SpectrumPointDto {
                wavelength_um: pt.wavelength_um,
                bandwidth_um: pt.bandwidth_um.is_finite().then_some(pt.bandwidth_um),
                value: pt.value,
                sigma: pt.sigma,
                obs_mjd: pt.obs_mjd,
                detector: pt.detector,
                flags: pt.flags.clone(),
            })
            .collect(),
    }))
}
