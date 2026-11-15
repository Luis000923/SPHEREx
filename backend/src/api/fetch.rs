//! Deadline-bounded parallel cutout fetching, shared by `/frames` and
//! `/spectrum` (task b5).
//!
//! Mirrors `IrsaClient::fetch_frames`: one task per selected observation,
//! while the client's semaphore still bounds actual upstream
//! concurrency — no extra parallelism is added here. The difference is
//! the total deadline: at expiry whatever decoded is returned so the API
//! can answer with `truncated = true` instead of failing outright
//! (D-014). Requests still in flight are NOT aborted: they keep running
//! in the background for up to [`BACKGROUND_GRACE`] so their results
//! land in the client cache and a Retry finds them (upstream work is
//! never thrown away; the client semaphore still bounds concurrency).
//!
//! [`search_until`] applies the same deadline to the SIA search, which
//! also keeps running in the background when it is too slow.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::{sleep_until, Instant};
use tracing::warn;

use crate::fits::Frame;
use crate::irsa::{CutoutFrame, IrsaError, Observation};

use super::FrameSource;

/// How long cutout fetches still in flight at the request deadline may
/// keep running in the background (filling the cache) before they are
/// aborted. Bounds the upstream work a burst of abandoned requests can
/// queue behind the shared semaphore.
/// Sized for IRSA under load: ~1 min per 4 MB cutout, 3 at a time, so
/// 24 cutouts need several minutes.
pub(crate) const BACKGROUND_GRACE: Duration = Duration::from_secs(600);

/// Fewest decoded frames that let a request stop at the soft deadline
/// (a time-lapse or blink needs two).
pub(crate) const SOFT_DEADLINE_MIN_FRAMES: usize = 2;

/// Outcome of [`search_until`] when the deadline wins.
pub(crate) enum SearchOutcome {
    /// The search finished in time.
    Done(Result<Arc<Vec<Observation>>, IrsaError>),
    /// The deadline expired first; the search continues detached.
    DeadlineHit,
}

/// Run the SIA search for `(ra, dec, fov)` until `until`. A search that
/// is still running at the deadline is detached (not cancelled) so its
/// result reaches the cache for the next request.
pub(crate) async fn search_until<S: FrameSource>(
    source: &Arc<S>,
    ra_deg: f64,
    dec_deg: f64,
    fov_deg: f64,
    until: Instant,
) -> SearchOutcome {
    let src = Arc::clone(source);
    let handle = tokio::spawn(async move { src.search(ra_deg, dec_deg, fov_deg).await });
    match tokio::time::timeout_at(until, handle).await {
        Ok(Ok(res)) => SearchOutcome::Done(res),
        Ok(Err(e)) => SearchOutcome::Done(Err(IrsaError::Internal(format!(
            "search task failed to join: {}",
            join_error_kind(&e)
        )))),
        // Dropping the JoinHandle detaches the task; it keeps running.
        Err(_) => SearchOutcome::DeadlineHit,
    }
}

/// Outcome of [`fetch_cutouts`].
pub(crate) struct FetchOutcome {
    /// Successfully decoded cutouts, in selection order.
    pub frames: Vec<CutoutFrame>,
    /// True when the deadline expired with requests still in flight.
    pub deadline_hit: bool,
    /// First per-cutout failure (surfaced when nothing decoded).
    pub first_error: Option<IrsaError>,
}

/// Fetch the cutout of every `selected` observation in parallel until
/// the instant `until`, or until `soft` once at least
/// [`SOFT_DEADLINE_MIN_FRAMES`] decoded. Individual failures are logged and skipped
/// (off-edge positions get HTTP 422 upstream — common); requests still
/// pending at the deadline continue in the background (see module docs).
pub(crate) async fn fetch_cutouts<S: FrameSource>(
    source: &Arc<S>,
    selected: Vec<Observation>,
    ra_deg: f64,
    dec_deg: f64,
    fov_deg: f64,
    soft: Instant,
    until: Instant,
) -> FetchOutcome {
    let mut set: JoinSet<(usize, Result<Arc<Frame>, IrsaError>)> = JoinSet::new();
    for (i, obs) in selected.iter().cloned().enumerate() {
        let src = Arc::clone(source);
        set.spawn(async move { (i, src.fetch_cutout(obs, ra_deg, dec_deg, fov_deg).await) });
    }

    let mut slots: Vec<Option<Arc<Frame>>> = Vec::new();
    slots.resize_with(selected.len(), || None);
    let mut first_error = None;
    let mut deadline_hit = false;
    let mut decoded = 0usize;
    while !set.is_empty() {
        tokio::select! {
            joined = set.join_next() => {
                match joined {
                    Some(Ok((i, Ok(frame)))) => {
                        slots[i] = Some(frame);
                        decoded += 1;
                    }
                    Some(Ok((_, Err(e)))) => {
                        // `IrsaError`'s Display can embed the upstream
                        // URL (path+query) inside `detail`; log the
                        // sanitised summary instead.
                        warn!("cutout skipped: {}", e.log_safe());
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                    }
                    Some(Err(e)) => {
                        warn!("cutout task failed to join: {}", join_error_kind(&e))
                    }
                    None => break,
                }
            }
            () = sleep_until(soft), if decoded >= SOFT_DEADLINE_MIN_FRAMES => {
                deadline_hit = true;
                break;
            }
            () = sleep_until(until) => {
                deadline_hit = true;
                break;
            }
        }
    }
    if deadline_hit && !set.is_empty() {
        finish_in_background(set);
    }

    let frames = selected
        .into_iter()
        .zip(slots)
        .filter_map(|(observation, slot)| slot.map(|frame| CutoutFrame { observation, frame }))
        .collect();
    FetchOutcome {
        frames,
        deadline_hit,
        first_error,
    }
}

/// Log-safe label for a failed task join (a panic payload could carry
/// an upstream URL; only the failure class is logged).
fn join_error_kind(e: &tokio::task::JoinError) -> &'static str {
    if e.is_panic() {
        "panic"
    } else if e.is_cancelled() {
        "cancelled"
    } else {
        "join failure"
    }
}

/// Let the remaining fetches of `set` complete (their results go to the
/// client cache) for at most [`BACKGROUND_GRACE`], then abort the rest.
fn finish_in_background<T: Send + 'static>(mut set: JoinSet<T>) {
    let pending = set.len();
    tokio::spawn(async move {
        let stop = Instant::now() + BACKGROUND_GRACE;
        loop {
            tokio::select! {
                joined = set.join_next() => {
                    if joined.is_none() {
                        break;
                    }
                }
                () = sleep_until(stop) => {
                    warn!("background cutout fetches aborted after grace period");
                    set.abort_all();
                    break;
                }
            }
        }
    });
    tracing::debug!("{pending} cutout fetch(es) continue in the background");
}
