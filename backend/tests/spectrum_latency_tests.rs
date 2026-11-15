//! Latency contract of `GET /api/v1/spectrum` (task b14).
//!
//! `/spectrum` shares the `/frames` fetch machinery: a cold request
//! answers at the soft deadline with whatever cutouts decoded (points
//! sorted by wavelength), the rest keeps downloading in the background
//! so the next identical request is complete and fast, and the response
//! is only an error when no point exists at all. Upstream positions are
//! quantised to a grid cell so nearby clicks share the client caches.
//!
//! Everything runs against [`LatencyStub`], a `FrameSource` double with
//! configurable per-cutout latency, bounded upstream concurrency and a
//! simulated client cache. No network.

mod fixtures;
mod support;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use spherex_backend::api::{build_router, AppState, FrameSource};
use spherex_backend::fits::{parse_frame, Frame};
use spherex_backend::irsa::{IrsaError, Observation};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tower::ServiceExt;

use support::{
    clone_irsa_error, cutout, obs_id_hash, stub_observations, CONTRACT_CODES, TEST_ADDR,
};

/// Decrement-on-drop marker for in-flight cold fetches.
struct FlightGuard(Arc<AtomicUsize>);

impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `FrameSource` double emulating `IrsaClient` without network:
///
/// - The first fetch of an observation at a position pays `latency`
///   under the bounded `upstream` semaphore (like
///   `IRSA_MAX_CONCURRENCY`); the same `(observation, ra, dec, fov)`
///   tuple is then a cache hit, including deterministic failures (the
///   client's negative cache).
/// - `search` pays `search_latency` once per distinct position tuple.
/// - `in_flight` counts cold fetches from the moment they start until
///   they finish, so tests can prove background work drains to zero.
struct LatencyStub {
    observations: Vec<Observation>,
    /// obs id hash -> decoded cutout served upstream.
    frames: HashMap<u64, Arc<Frame>>,
    /// obs id hashes whose fetch always fails (upstream 422, the
    /// "Arrays do not overlap" of a point just off the array).
    fails: HashSet<u64>,
    /// Upstream latency of one cold cutout fetch.
    latency: Duration,
    /// Upstream latency of one cold SIA search.
    search_latency: Duration,
    /// Error returned by every `search` (e.g. an upstream outage).
    search_error: Option<IrsaError>,
    /// Bounds simultaneous cold fetches.
    upstream: Semaphore,
    /// "Client cache": `(obs hash, ra, dec, fov)` tuples already served.
    cached: Mutex<HashSet<(u64, u64, u64, u64)>>,
    /// Search `(ra, dec, fov)` tuples already answered once.
    searched: Mutex<HashSet<(u64, u64, u64)>>,
    /// Cold fetches started but not finished.
    in_flight: Arc<AtomicUsize>,
    /// Total cold fetches performed.
    upstream_calls: AtomicUsize,
    /// `fetch_cutout` invocations seen (cache hits included).
    fetch_calls: AtomicUsize,
    /// `search` invocations seen.
    search_calls: AtomicUsize,
    /// Positions `fetch_cutout` / `search` were invoked with, as f64
    /// bit patterns.
    fetch_positions: Mutex<Vec<(u64, u64, u64)>>,
    search_positions: Mutex<Vec<(u64, u64, u64)>>,
}

impl LatencyStub {
    /// `n` observations, each decoding to a constant-flux cutout with a
    /// distinct central wavelength (`1.0 + 0.3 i` um) so a spectrum has
    /// `n` different points.
    fn new(n: usize, latency: Duration, upstream_permits: usize) -> Self {
        let observations = stub_observations(n);
        let frames = observations
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let bytes = cutout(1.0, (i % 6 + 1) as i64, 1.0 + i as f32 * 0.3);
                (
                    obs_id_hash(o),
                    Arc::new(parse_frame(&bytes).expect("fixture parses")),
                )
            })
            .collect();
        Self {
            observations,
            frames,
            fails: HashSet::new(),
            latency,
            search_latency: Duration::ZERO,
            search_error: None,
            upstream: Semaphore::new(upstream_permits.max(1)),
            cached: Mutex::new(HashSet::new()),
            searched: Mutex::new(HashSet::new()),
            in_flight: Arc::new(AtomicUsize::new(0)),
            upstream_calls: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
            search_calls: AtomicUsize::new(0),
            fetch_positions: Mutex::new(Vec::new()),
            search_positions: Mutex::new(Vec::new()),
        }
    }

    /// Answer for `hash`, on a cache hit or after a cold fetch.
    fn served(&self, hash: u64) -> Result<Arc<Frame>, IrsaError> {
        if self.fails.contains(&hash) {
            return Err(IrsaError::Http { status: 422 });
        }
        self.frames
            .get(&hash)
            .cloned()
            .ok_or(IrsaError::Http { status: 422 })
    }
}

impl FrameSource for LatencyStub {
    async fn search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Arc<Vec<Observation>>, IrsaError> {
        self.search_calls.fetch_add(1, Ordering::SeqCst);
        let key = (ra_deg.to_bits(), dec_deg.to_bits(), fov_deg.to_bits());
        self.search_positions.lock().expect("positions").push(key);
        // First sight of this position pays the upstream latency (and
        // then the result would sit in the client cache).
        if self.searched.lock().expect("searched").insert(key) && !self.search_latency.is_zero() {
            tokio::time::sleep(self.search_latency).await;
        }
        if let Some(e) = &self.search_error {
            return Err(clone_irsa_error(e));
        }
        if self.observations.is_empty() {
            return Err(IrsaError::NoObservations);
        }
        Ok(Arc::new(self.observations.clone()))
    }

    async fn fetch_cutout(
        &self,
        observation: Observation,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Arc<Frame>, IrsaError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        let key = (
            obs_id_hash(&observation),
            ra_deg.to_bits(),
            dec_deg.to_bits(),
            fov_deg.to_bits(),
        );
        self.fetch_positions
            .lock()
            .expect("positions")
            .push((key.1, key.2, key.3));
        // A tuple already served is a client-cache hit (instant).
        if !self.cached.lock().expect("cache").insert(key) {
            return self.served(key.0);
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let _in_flight = FlightGuard(Arc::clone(&self.in_flight));
        let _permit = self.upstream.acquire().await.expect("semaphore open");
        tokio::time::sleep(self.latency).await;
        self.upstream_calls.fetch_add(1, Ordering::SeqCst);
        self.served(key.0)
    }
}

/// `AppState` for the stub with explicit deadlines.
fn state(source: Arc<LatencyStub>, soft: Duration, hard: Duration) -> AppState<LatencyStub> {
    AppState {
        source,
        collections: Arc::new(vec!["spherex_qr3".to_string()]),
        static_dir: "frontend/dist".into(),
        allowed_origins: Arc::new(Vec::new()),
        output_npix: 16,
        max_frames_cap: 60,
        request_deadline: hard,
        soft_deadline: soft,
        rate_limit_rps: 1_000_000,
    }
}

/// GET `/api/v1/spectrum?ra=<ra>&dec=<dec>` through the router; returns
/// the status, the parsed JSON body and the wall-clock latency.
async fn spectrum(
    state: &AppState<LatencyStub>,
    ra: &str,
    dec: &str,
) -> (StatusCode, Value, Duration) {
    let app = build_router(state.clone());
    let mut req = Request::builder()
        .uri(format!("/api/v1/spectrum?ra={ra}&dec={dec}"))
        .body(Body::empty())
        .expect("request builds");
    req.extensions_mut().insert(ConnectInfo(TEST_ADDR));
    let t0 = Instant::now();
    let res = app.oneshot(req).await.expect("router answers");
    let elapsed = t0.elapsed();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads");
    let v: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({"_raw": "not json"}));
    (status, v, elapsed)
}

fn error_code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

/// Wait until the stub's in-flight counter returns to 0 (all background
/// fetches drained), bounded by `budget`.
async fn wait_drained(stub: &LatencyStub, budget: Duration) {
    let t0 = Instant::now();
    while stub.in_flight.load(Ordering::SeqCst) > 0 {
        assert!(
            t0.elapsed() < budget,
            "background fetches did not drain within {budget:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_sorted_by_wavelength(points: &[Value]) {
    let waves: Vec<f64> = points
        .iter()
        .map(|p| p["wavelength_um"].as_f64().expect("wavelength_um"))
        .collect();
    assert!(
        waves.windows(2).all(|w| w[0] <= w[1]),
        "points not sorted by wavelength: {waves:?}"
    );
}

// ---------------------------------------------------------------------
// Soft deadline + background completion
// ---------------------------------------------------------------------

/// Cold request: 12 cutouts at 3 s each under a 3-way upstream bound
/// (batches decode at ~3, 6, 9, 12 s). The handler must answer at the
/// soft deadline (4 s) with the first decoded batch (>= 2 points),
/// sorted by wavelength, while the rest keeps downloading.
#[tokio::test]
async fn cold_request_answers_at_soft_deadline_with_partial_points() {
    let stub = Arc::new(LatencyStub::new(12, Duration::from_secs(3), 3));
    let st = state(
        Arc::clone(&stub),
        Duration::from_secs(4),
        Duration::from_secs(30),
    );
    let (status, v, elapsed) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let points = v["points"].as_array().expect("points array");
    // Only the first 3-way batch can have decoded by the soft deadline.
    assert_eq!(points.len(), 3, "{v}");
    assert!(
        elapsed < Duration::from_secs(6),
        "response did not honour the soft deadline: {elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(3),
        "answered before the first cutout could decode: {elapsed:?}"
    );
    assert_sorted_by_wavelength(points);
    // Contract shape: no partial indicator exists, so none is emitted.
    for key in ["ra_deg", "dec_deg", "unit", "points"] {
        assert!(v.get(key).is_some(), "missing {key} in {v}");
    }
}

/// The downloads abandoned at the soft deadline keep running: every
/// spawned fetch finishes and the in-flight counter returns to 0 (no
/// unbounded background tasks), each observation fetched exactly once.
#[tokio::test]
async fn background_fetches_drain_to_zero() {
    let stub = Arc::new(LatencyStub::new(12, Duration::from_secs(1), 4));
    let st = state(
        Arc::clone(&stub),
        Duration::from_millis(1200),
        Duration::from_secs(20),
    );
    let (status, v, _) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    wait_drained(&stub, Duration::from_secs(15)).await;
    assert_eq!(stub.in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(stub.upstream_calls.load(Ordering::SeqCst), 12);
    assert_eq!(stub.fetch_calls.load(Ordering::SeqCst), 12);
}

/// After the background drain the same request is a cache hit: the full
/// spectrum returns quickly.
#[tokio::test]
async fn second_identical_request_is_complete_and_fast() {
    let stub = Arc::new(LatencyStub::new(12, Duration::from_secs(3), 3));
    let st = state(
        Arc::clone(&stub),
        Duration::from_secs(4),
        Duration::from_secs(30),
    );
    let (status, v, _) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    wait_drained(&stub, Duration::from_secs(20)).await;

    let (status, v, elapsed) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let points = v["points"].as_array().expect("points array");
    assert_eq!(points.len(), 12, "{v}");
    assert_sorted_by_wavelength(points);
    assert!(
        elapsed < Duration::from_secs(2),
        "warm spectrum should be fast, took {elapsed:?}"
    );
    assert_eq!(
        stub.upstream_calls.load(Ordering::SeqCst),
        12,
        "cached cutouts were re-downloaded"
    );
}

// ---------------------------------------------------------------------
// Cache coalescing across clicks
// ---------------------------------------------------------------------

/// Clicks a few arcseconds apart are quantised to the same upstream
/// position: the second click reuses the cached search and cutouts
/// instead of paying another cold fetch. A click in the next cell pays
/// again (the tuple is part of the key, as in the real client).
#[tokio::test]
async fn nearby_clicks_share_the_upstream_cache() {
    let stub = Arc::new(LatencyStub::new(4, Duration::from_millis(300), 4));
    let st = state(
        Arc::clone(&stub),
        Duration::from_secs(5),
        Duration::from_secs(10),
    );
    let (status, v, _) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(stub.upstream_calls.load(Ordering::SeqCst), 4);

    // ~10" away: same snap cell, full spectrum, no new upstream fetch.
    let (status, v, elapsed) = spectrum(&st, "337.881", "-5.188").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["points"].as_array().map(Vec::len), Some(4));
    assert!(
        elapsed < Duration::from_millis(300),
        "nearby click paid a cold fetch: {elapsed:?}"
    );
    assert_eq!(
        stub.upstream_calls.load(Ordering::SeqCst),
        4,
        "nearby click re-downloaded cutouts"
    );

    // Both clicks went upstream at the same quantised position.
    let snap = |v: f64| (v / 0.005f64).round() * 0.005;
    let cell = (
        snap(337.878f64).to_bits(),
        snap(-5.189f64).to_bits(),
        0.01f64.to_bits(),
    );
    let fetch_pos = stub.fetch_positions.lock().expect("positions").clone();
    assert!(
        fetch_pos.iter().all(|p| *p == cell),
        "upstream fetch positions differ: {fetch_pos:?}"
    );
    let search_pos = stub.search_positions.lock().expect("positions").clone();
    assert!(
        search_pos
            .iter()
            .all(|&(ra, dec, _)| (ra, dec) == (cell.0, cell.1)),
        "upstream search positions differ: {search_pos:?}"
    );

    // A click in the next cell is a different cache tuple and fetches
    // again (still inside the synthetic cutout).
    let (status, v, _) = spectrum(&st, "337.8745", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["points"].as_array().map(Vec::len), Some(4));
    assert_eq!(stub.upstream_calls.load(Ordering::SeqCst), 8);
}

// ---------------------------------------------------------------------
// Failure isolation and contract errors
// ---------------------------------------------------------------------

/// Some cutouts failing upstream (e.g. the 422 "Arrays do not overlap"
/// near an array edge) never turns the response into an error while
/// other points decode.
#[tokio::test]
async fn failed_cutouts_do_not_error_while_others_succeed() {
    let mut stub = LatencyStub::new(6, Duration::from_millis(50), 6);
    for o in &stub.observations[2..4] {
        stub.fails.insert(obs_id_hash(o));
    }
    let stub = Arc::new(stub);
    let st = state(
        Arc::clone(&stub),
        Duration::from_secs(5),
        Duration::from_secs(10),
    );
    let (status, v, _) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let points = v["points"].as_array().expect("points array");
    assert_eq!(points.len(), 4, "{v}");
    assert_sorted_by_wavelength(points);
}

/// When every selected cutout fails, the answer is a contract
/// `ErrorBody` (deterministic upstream 4xx maps to `internal`), never a
/// partial 200.
#[tokio::test]
async fn all_cutouts_failing_returns_the_contract_error() {
    let mut stub = LatencyStub::new(4, Duration::from_millis(10), 4);
    for o in &stub.observations {
        stub.fails.insert(obs_id_hash(o));
    }
    let stub = Arc::new(stub);
    let st = state(
        Arc::clone(&stub),
        Duration::from_secs(5),
        Duration::from_secs(10),
    );
    let (status, v, _) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
    let code = error_code(&v);
    assert!(
        CONTRACT_CODES.contains(&code),
        "non-contract error code {code:?} in {v}"
    );
    assert_eq!(code, "internal", "{v}");
}

/// Nothing decoded by the deadline: a 502 `upstream_unavailable`, and
/// the fetches still in flight are NOT aborted (they keep filling the
/// cache for the retry).
#[tokio::test]
async fn nothing_decoded_by_the_deadline_is_502_and_fetches_continue() {
    let stub = Arc::new(LatencyStub::new(4, Duration::from_secs(60), 4));
    let st = state(
        Arc::clone(&stub),
        Duration::from_millis(50),
        Duration::from_millis(50),
    );
    let (status, v, elapsed) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(error_code(&v), "upstream_unavailable");
    assert!(
        elapsed < Duration::from_secs(2),
        "deadline ignored: {elapsed:?}"
    );
    assert!(
        stub.in_flight.load(Ordering::SeqCst) > 0,
        "in-flight cutouts were aborted at the deadline"
    );
}

/// A search slower than the deadline counts against it (the detached
/// search keeps running to fill the cache): JSON 502.
#[tokio::test]
async fn slow_search_counts_against_the_deadline() {
    let mut stub = LatencyStub::new(4, Duration::ZERO, 4);
    stub.search_latency = Duration::from_secs(60);
    let stub = Arc::new(stub);
    let st = state(
        Arc::clone(&stub),
        Duration::from_millis(50),
        Duration::from_millis(50),
    );
    let (status, v, elapsed) = spectrum(&st, "337.878", "-5.189").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(error_code(&v), "upstream_unavailable");
    assert!(
        elapsed < Duration::from_secs(2),
        "search ignored the deadline: {elapsed:?}"
    );
}
