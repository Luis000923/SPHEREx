//! `GET /api/v1/spectrum` (task b5).
//!
//! Validates `ra`/`dec`/`aperture_px`, fetches the cutouts covering the
//! point on the smallest allowed FOV under the request deadline, then
//! calls [`extract_spectrum`]. An empty extraction is a 404
//! (`not_found`), exactly as the contract specifies.

use axum::{extract::State, http::Uri, Json};

use crate::error::AppError;
use crate::fits::Frame;
use crate::irsa::select_frames;
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
    let observations =
        match search_until(&state.source, p.ra_deg, p.dec_deg, SPECTRUM_FOV_DEG, until).await {
            SearchOutcome::Done(res) => res.map_err(AppError::from)?,
            SearchOutcome::DeadlineHit => return Err(AppError::UpstreamUnavailable),
        };
    let selected = select_frames(&observations, state.max_frames_cap.min(SPECTRUM_MAX_FRAMES));
    let fetched = fetch_cutouts(
        &state.source,
        selected,
        p.ra_deg,
        p.dec_deg,
        SPECTRUM_FOV_DEG,
        soft,
        until,
    )
    .await;
    if fetched.frames.is_empty() {
        return Err(if fetched.deadline_hit {
            AppError::UpstreamUnavailable
        } else {
            fetched
                .first_error
                .map_or(AppError::NotFound, AppError::from)
        });
    }

    let refs: Vec<&Frame> = fetched.frames.iter().map(|c| c.frame.as_ref()).collect();
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
