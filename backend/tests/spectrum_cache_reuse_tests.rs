//! Cache-reuse contract of `GET /api/v1/spectrum` (task b18).
//!
//! A click inside a field `/frames` already downloaded — or one baked
//! into `CACHE_DIR` — must be assembled from those cached cutouts with
//! zero upstream traffic (the observation list comes from a cached SIA
//! cone containing the click, each frame from the smallest cached
//! cutout containing the click plus the aperture margin). Clicks
//! outside the cached field, or whose aperture would fall off the
//! cached cutout, fall back to the normal deadline-bounded download.
//!
//! Coverage:
//! (a) /frames then /spectrum inside the field: zero search/cutout
//!     calls, points equal a direct `extract_spectrum`.
//! (b) a click outside the cached field downloads as before.
//! (c) an edge click whose aperture would fall off is not served from
//!     that cutout.
//! (d) `CACHE_DIR` + a new client instance (restart): disk-cached
//!     cutouts and the SIA result are found, still zero upstream.
//! (e) the secondary coverage index stays inside its documented bound.
//! (f) concurrent identical requests coalesce to one upstream fetch.
//!
//! No test reaches the real IRSA: the first three use a `FrameSource`
//! stub modelling the client coverage cache, the last three use the
//! real `IrsaClient` against a loopback stub server.

mod fixtures;
mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use serde_json::{json, Value};
use spherex_backend::api::{build_router, AppState, FrameSource};
use spherex_backend::fits::{parse_frame, Frame};
use spherex_backend::irsa::{
    search_radius_deg, select_frames, IrsaClient, IrsaConfig, IrsaError, Observation,
};
use spherex_backend::science::extract_spectrum;
use tokio::time::Instant;
use tower::ServiceExt;

use fixtures::*;
use support::{obs_id_hash, stub_observations, TEST_ADDR};

/// Position shared by the fixtures: the same centre `support::cutout`
/// uses so helper maths stays in one place.
const C_RA: f64 = 337.878;
const C_DEC: f64 = -5.189;

/// Synthetic MEF cutout `nx`x`nx` centred on `(ra, dec)` with plate
/// scale `span_deg / nx`, constant `flux`/`variance`/`zodi`, detector
/// and a 2x2-control-point WCS-WAVE map around `wave_um`.
fn cutout_at(
    ra: f64,
    dec: f64,
    nx: usize,
    span_deg: f64,
    flux: f32,
    detector: i64,
    wave_um: f32,
) -> Vec<u8> {
    let ny = nx;
    let scale = span_deg / nx as f64;
    let wcs = vec![
        card_str("CTYPE1", "RA---TAN"),
        card_str("CTYPE2", "DEC--TAN"),
        card_flt("CRPIX1", nx as f64 / 2.0 + 0.5),
        card_flt("CRPIX2", ny as f64 / 2.0 + 0.5),
        card_flt("CRVAL1", ra),
        card_flt("CRVAL2", dec),
        card_flt("CD1_1", -scale),
        card_flt("CD1_2", 0.0),
        card_flt("CD2_1", 0.0),
        card_flt("CD2_2", scale),
        card_int("DETECTOR", detector),
        card_str("BUNIT", "MJy / sr"),
    ];
    let legend = vec![
        card_str("MSKN0000", "TRANSIENT"),
        card_str("MSKD0000", "Transient source in the pixel"),
    ];
    let mut f = primary_hdu();
    f.extend(image_hdu(
        "IMAGE",
        -32,
        nx,
        ny,
        &f32_data(&vec![flux; nx * ny]),
        &wcs,
    ));
    f.extend(image_hdu(
        "FLAGS",
        32,
        nx,
        ny,
        &i32_data(&vec![0; nx * ny]),
        &legend,
    ));
    f.extend(image_hdu(
        "VARIANCE",
        -32,
        nx,
        ny,
        &f32_data(&vec![0.5; nx * ny]),
        &[],
    ));
    f.extend(image_hdu(
        "ZODI",
        -32,
        nx,
        ny,
        &f32_data(&vec![0.1; nx * ny]),
        &[],
    ));
    f.extend(wcs_wave_hdu(
        &[1, nx as i32],
        &[1, ny as i32],
        &[
            wave_um,
            0.05,
            wave_um + 0.01,
            0.05,
            wave_um,
            0.05,
            wave_um + 0.01,
            0.05,
        ],
        &[],
    ));
    f
}

/// Angular distance (deg), same small-angle metric the client uses.
fn sky_dist(ra: f64, dec: f64, cra: f64, cdec: f64) -> f64 {
    let dra = (ra - cra + 540.0).rem_euclid(360.0) - 180.0;
    let ddec = dec - cdec;
    let x = dra * f64::midpoint(dec, cdec).to_radians().cos();
    x.hypot(ddec)
}

/// Same pixel-space coverage rule as the client: the point plus a
/// `margin_deg`-wide aperture must fit inside the array.
fn covers(frame: &Frame, ra: f64, dec: f64, margin_deg: f64) -> bool {
    let cd = frame.wcs.cd;
    let scale = (cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]).abs().sqrt();
    let Ok((px, py)) = frame.wcs.radec_to_pixel(ra, dec) else {
        return false;
    };
    let (ny, nx) = frame.image.dim();
    let m = margin_deg / scale;
    px.is_finite()
        && py.is_finite()
        && px - m >= 0.5
        && px + m <= nx as f64 + 0.5
        && py - m >= 0.5
        && py + m <= ny as f64 + 0.5
}

/// Key of a "downloaded" cutout: (obs hash, ra bits, dec bits, fov
/// bits) — the same tuple the client cache keys on.
type ServedKey = (u64, u64, u64, u64);

/// `FrameSource` double modelling the real client's coverage cache:
/// `fetch_cutout` "downloads" a cutout centred on the requested
/// position and records it in `served`, keyed like the client cache;
/// `cached_cutout` probes exactly what the client probe would find
/// (geometry filter, then the pixel-space aperture check) without
/// touching the counted upstream path.
struct CachingStub {
    observations: Vec<Observation>,
    /// Central wavelength per observation index (keeps points distinct
    /// and sorted).
    waves: Vec<f32>,
    /// Raster side of every served cutout, px.
    cut_n: usize,
    /// served key -> decoded cutout.
    served: Mutex<HashMap<ServedKey, Arc<Frame>>>,
    /// Search cones answered: (ra, dec, radius) degrees.
    cones: Mutex<Vec<(f64, f64, f64)>>,
    /// `search` invocations (counted as upstream traffic).
    search_calls: AtomicUsize,
    /// `fetch_cutout` invocations (counted as upstream traffic).
    fetch_calls: AtomicUsize,
}

impl CachingStub {
    /// `n` observations whose cutouts are `cut_n`x`cut_n` rasters.
    fn new(n: usize, cut_n: usize) -> Self {
        Self {
            observations: stub_observations(n),
            waves: (0..n).map(|i| 1.0 + 0.05 * i as f32).collect(),
            cut_n,
            served: Mutex::new(HashMap::new()),
            cones: Mutex::new(Vec::new()),
            search_calls: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
        }
    }

    /// Wavelength for the observation (by position in the list).
    fn wave_of(&self, obs: &Observation) -> f32 {
        let i = self
            .observations
            .iter()
            .position(|o| o.access_url == obs.access_url)
            .unwrap_or(0);
        self.waves[i]
    }

    /// Frame the client would have cached for `(obs, ra, dec, fov)`.
    fn served_frame(&self, obs: &Observation, ra: f64, dec: f64, fov: f64) -> Option<Arc<Frame>> {
        self.served
            .lock()
            .expect("served")
            .get(&(obs_id_hash(obs), ra.to_bits(), dec.to_bits(), fov.to_bits()))
            .cloned()
    }
}

impl FrameSource for CachingStub {
    async fn search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Arc<Vec<Observation>>, IrsaError> {
        self.search_calls.fetch_add(1, Ordering::SeqCst);
        self.cones
            .lock()
            .expect("cones")
            .push((ra_deg, dec_deg, search_radius_deg(fov_deg)));
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
        if let Some(f) = self.served.lock().expect("served").get(&key) {
            return Ok(Arc::clone(f));
        }
        let wave = self.wave_of(&observation);
        let detector = self
            .observations
            .iter()
            .position(|o| o.access_url == observation.access_url)
            .unwrap_or(0) as i64
            % 6
            + 1;
        let bytes = cutout_at(ra_deg, dec_deg, self.cut_n, fov_deg, 1.0, detector, wave);
        let frame = Arc::new(parse_frame(&bytes).expect("fixture parses"));
        self.served
            .lock()
            .expect("served")
            .insert(key, Arc::clone(&frame));
        Ok(frame)
    }

    async fn cached_cutout(
        &self,
        observation: Observation,
        ra_deg: f64,
        dec_deg: f64,
        margin_deg: f64,
    ) -> Option<Arc<Frame>> {
        let obs = obs_id_hash(&observation);
        let mut candidates: Vec<(f64, Arc<Frame>)> = self
            .served
            .lock()
            .expect("served")
            .iter()
            .filter(|((o, ra, dec, fov), _)| {
                *o == obs
                    && sky_dist(ra_deg, dec_deg, f64::from_bits(*ra), f64::from_bits(*dec))
                        <= search_radius_deg(f64::from_bits(*fov))
            })
            .map(|((_, _, _, fov), f)| (f64::from_bits(*fov), Arc::clone(f)))
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, frame) in candidates {
            if covers(&frame, ra_deg, dec_deg, margin_deg) {
                return Some(frame);
            }
        }
        None
    }

    async fn cached_observations(
        &self,
        ra_deg: f64,
        dec_deg: f64,
    ) -> Option<Arc<Vec<Observation>>> {
        let covered = self
            .cones
            .lock()
            .expect("cones")
            .iter()
            .any(|&(ra, dec, radius)| sky_dist(ra_deg, dec_deg, ra, dec) <= radius);
        covered.then(|| Arc::new(self.observations.clone()))
    }
}

/// `AppState` over the stub with production-like deadlines.
fn stub_state(source: Arc<CachingStub>) -> AppState<CachingStub> {
    AppState {
        source,
        collections: Arc::new(vec!["spherex_qr3".to_string()]),
        static_dir: "frontend/dist".into(),
        allowed_origins: Arc::new(Vec::new()),
        output_npix: 16,
        max_frames_cap: 60,
        request_deadline: Duration::from_secs(10),
        soft_deadline: Duration::from_secs(10),
        rate_limit_rps: 1_000_000,
    }
}

/// GET through the router; returns (status, body, wall time).
async fn call<S: FrameSource>(state: &AppState<S>, uri: &str) -> (StatusCode, Value, Duration) {
    let app = build_router(state.clone());
    let mut req = Request::builder()
        .uri(uri)
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

// ---------------------------------------------------------------------
// (a) /frames warms the field; /spectrum inside it costs zero upstream
// ---------------------------------------------------------------------

/// After `/frames` downloads a field, a click anywhere inside it must
/// be answered from the cached cutouts alone: no SIA search, no cutout
/// downloads, and the points are exactly what `extract_spectrum`
/// computes over the cached frames.
#[tokio::test]
async fn spectrum_inside_cached_field_is_fully_served_from_cache() {
    let stub = Arc::new(CachingStub::new(30, 40));
    let st = stub_state(Arc::clone(&stub));
    // 0.05 deg / 40 px = 0.00125 deg/px, half-side 0.025 deg.
    let (status, v, _) = call(
        &st,
        &format!("/api/v1/frames?ra={C_RA}&dec={C_DEC}&fov=0.05&mode=raw"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (searches, fetches) = (
        stub.search_calls.load(Ordering::SeqCst),
        stub.fetch_calls.load(Ordering::SeqCst),
    );
    assert!(searches >= 1 && fetches >= 1, "warm-up did not fetch");

    // Click ~10" from the frames centre: inside the field, different
    // position (no bit-identical cache key exists for it).
    let click = (C_RA + 0.008, C_DEC + 0.006);
    let (status, v, elapsed) = call(
        &st,
        &format!(
            "/api/v1/spectrum?ra={}&dec={}&aperture_px=3",
            click.0, click.1
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        stub.search_calls.load(Ordering::SeqCst),
        searches,
        "spectrum ran an SIA search although a cached cone covers the click"
    );
    assert_eq!(
        stub.fetch_calls.load(Ordering::SeqCst),
        fetches,
        "spectrum downloaded cutouts although cached ones cover the click"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "cached spectrum took {elapsed:?}"
    );

    // Expected points: extract_spectrum over exactly the frames the
    // same selection would have cached (24 of the 30 observations).
    let selected = select_frames(&stub.observations, 24);
    let frames: Vec<Arc<Frame>> = selected
        .iter()
        .map(|o| {
            stub.served_frame(o, C_RA, C_DEC, 0.05)
                .expect("frame cached")
        })
        .collect();
    let refs: Vec<&Frame> = frames.iter().map(Arc::as_ref).collect();
    let expected = extract_spectrum(&refs, click.0, click.1, 3, true).expect("extract");

    let points = v["points"].as_array().expect("points array");
    assert_eq!(points.len(), expected.len(), "{v}");
    for (got, want) in points.iter().zip(&expected) {
        assert!(
            (got["wavelength_um"].as_f64().expect("w") - want.wavelength_um).abs() < 1e-9,
            "wavelength mismatch: {got} vs {want:?}"
        );
        assert!(
            (got["value"].as_f64().expect("v") - want.value).abs() < 1e-9,
            "flux mismatch: {got} vs {want:?}"
        );
    }
    let waves: Vec<f64> = points
        .iter()
        .map(|p| p["wavelength_um"].as_f64().expect("w"))
        .collect();
    assert!(
        waves.windows(2).all(|w| w[0] <= w[1]),
        "points not sorted by wavelength"
    );
}

// ---------------------------------------------------------------------
// (b) outside the field: normal download path
// ---------------------------------------------------------------------

/// A click outside every cached cone and cutout pays the normal
/// upstream cost: one search plus one cutout per selected observation.
#[tokio::test]
async fn spectrum_outside_cached_field_downloads_as_before() {
    let stub = Arc::new(CachingStub::new(6, 40));
    let st = stub_state(Arc::clone(&stub));
    let (status, v, _) = call(
        &st,
        &format!("/api/v1/frames?ra={C_RA}&dec={C_DEC}&fov=0.05&mode=raw"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (searches, fetches) = (
        stub.search_calls.load(Ordering::SeqCst),
        stub.fetch_calls.load(Ordering::SeqCst),
    );

    // ~0.6 deg away: outside the 0.035-deg search cone and the
    // 0.05-deg cutouts.
    let (status, v, _) = call(
        &st,
        &format!("/api/v1/spectrum?ra={}&dec={}", C_RA + 0.6, C_DEC + 0.2),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(
        stub.search_calls.load(Ordering::SeqCst) > searches,
        "outside click reused the search cache"
    );
    assert!(
        stub.fetch_calls.load(Ordering::SeqCst) > fetches,
        "outside click reused the cutout cache"
    );
    assert!(v["points"].as_array().is_some_and(|p| !p.is_empty()), "{v}");
}

// ---------------------------------------------------------------------
// (c) edge click: aperture falling off must not be served from cache
// ---------------------------------------------------------------------

/// A click inside the cutout box but closer to the edge than the
/// aperture needs is NOT served from that cutout: the handler falls
/// back to downloading (an aperture centred there would fall off the
/// array, producing a clipped spectrum).
#[tokio::test]
async fn edge_click_with_aperture_off_the_cutout_is_not_served_cached() {
    let stub = Arc::new(CachingStub::new(6, 40));
    let st = stub_state(Arc::clone(&stub));
    let (status, v, _) = call(
        &st,
        &format!("/api/v1/frames?ra={C_RA}&dec={C_DEC}&fov=0.05&mode=raw"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let fetches = stub.fetch_calls.load(Ordering::SeqCst);

    // Cached cutouts: 40 px at 0.00125 deg/px, pixel centre 20.5. The
    // 5-px aperture needs the click ~5 px inside -> allowed pixel
    // range is roughly [5.5, 35.5]. dec offset +0.0215 maps to py~37.7:
    // inside the array but inside the edge margin -> must refetch.
    let (status, v, _) = call(
        &st,
        &format!(
            "/api/v1/spectrum?ra={}&dec={}&aperture_px=5",
            C_RA,
            C_DEC + 0.0215
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(
        stub.fetch_calls.load(Ordering::SeqCst) > fetches,
        "edge click was served from a cutout that does not contain the aperture"
    );
}

// ---------------------------------------------------------------------
// Loopback upstream stub for the real-client tests (d, e, f)
// ---------------------------------------------------------------------

/// Minimal SIA VOTable with `urls.len()` rows (same shape the b2 tests
/// verified against the real service).
fn sia_votable(urls: &[String]) -> String {
    let mut rows = String::new();
    for (i, u) in urls.iter().enumerate() {
        let t0 = 61000.0 + i as f64;
        rows.push_str(&format!(
            "<TR><TD>{t0}</TD><TD>{}</TD><TD>1.10e-6</TD><TD>1.117e-6</TD>\
             <TD>SPHEREx-D{}</TD><TD>{u}</TD></TR>",
            t0 + 0.0014,
            i % 6 + 1
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <VOTABLE version=\"1.3\" xmlns=\"http://www.ivoa.net/xml/VOTable/v1.3\">\
         <RESOURCE type=\"results\"><TABLE>\
         <FIELD name=\"t_min\" datatype=\"double\" unit=\"d\"/>\
         <FIELD name=\"t_max\" datatype=\"double\" unit=\"d\"/>\
         <FIELD name=\"em_min\" datatype=\"double\" unit=\"m\"/>\
         <FIELD name=\"em_max\" datatype=\"double\" unit=\"m\"/>\
         <FIELD name=\"energy_bandpassname\" datatype=\"char\" arraysize=\"*\"/>\
         <FIELD name=\"access_url\" datatype=\"char\" arraysize=\"*\"/>\
         <DATA><TABLEDATA>{rows}</TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>"
    )
}

/// Loopback stub state: counts SIA and cutout hits; serves a fixed
/// large-enough cutout regardless of the cutout parameters.
struct UpStub {
    sia_hits: AtomicUsize,
    ibe_hits: AtomicUsize,
    sia_body: String,
    fits: Vec<u8>,
    ibe_delay: Duration,
}

async fn sia_handler(
    axum::extract::State(s): axum::extract::State<Arc<UpStub>>,
) -> axum::response::Response {
    s.sia_hits.fetch_add(1, Ordering::SeqCst);
    (StatusCode::OK, s.sia_body.clone()).into_response()
}

async fn ibe_handler(
    axum::extract::State(s): axum::extract::State<Arc<UpStub>>,
) -> axum::response::Response {
    s.ibe_hits.fetch_add(1, Ordering::SeqCst);
    if !s.ibe_delay.is_zero() {
        tokio::time::sleep(s.ibe_delay).await;
    }
    (StatusCode::OK, s.fits.clone()).into_response()
}

/// Bind the upstream stub on an ephemeral port; returns (base, state).
async fn start_upstream(n_obs: usize, fits: Vec<u8>, ibe_delay: Duration) -> (String, Arc<UpStub>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("local addr");
    let base = format!("http://{addr}");
    let urls: Vec<String> = (0..n_obs)
        .map(|i| format!("{base}/ibe/f{i}.fits"))
        .collect();
    let stub = Arc::new(UpStub {
        sia_hits: AtomicUsize::new(0),
        ibe_hits: AtomicUsize::new(0),
        sia_body: sia_votable(&urls),
        fits,
        ibe_delay,
    });
    let app = axum::Router::new()
        .route("/SIA", axum::routing::get(sia_handler))
        .route("/ibe/*rest", axum::routing::get(ibe_handler))
        .with_state(Arc::clone(&stub));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, stub)
}

/// Client config pointed at the loopback stub with fast retries.
fn live_cfg(base: &str, cache_dir: Option<std::path::PathBuf>) -> IrsaConfig {
    IrsaConfig {
        base_url: base.to_string(),
        collections: vec!["spherex_qr3".to_string()],
        retry_base_delay: Duration::from_millis(1),
        request_timeout: Duration::from_secs(5),
        cache_dir,
        ..IrsaConfig::default()
    }
}

/// `AppState` over a real `IrsaClient` (the production wiring).
fn live_state(client: IrsaClient) -> AppState<IrsaClient> {
    AppState {
        source: Arc::new(client),
        collections: Arc::new(vec!["spherex_qr3".to_string()]),
        static_dir: "frontend/dist".into(),
        allowed_origins: Arc::new(Vec::new()),
        output_npix: 16,
        max_frames_cap: 60,
        request_deadline: Duration::from_secs(30),
        soft_deadline: Duration::from_secs(30),
        rate_limit_rps: 1_000_000,
    }
}

// ---------------------------------------------------------------------
// (d) CACHE_DIR restart: baked cutouts serve /spectrum offline
// ---------------------------------------------------------------------

/// With `CACHE_DIR` set, a fresh client (a restarted process) finds the
/// disk-cached SIA result and cutouts through the coverage index and
/// answers `/spectrum` inside a baked field with zero upstream calls.
#[tokio::test]
async fn disk_cached_cutouts_serve_spectrum_after_restart() {
    let dir = std::env::temp_dir().join(format!(
        "spherex-b18-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // 48x48 px at 0.001 deg/px -> 0.048 deg field centred at (C_RA,C_DEC).
    let fits = cutout_at(C_RA, C_DEC, 48, 0.048, 1.0, 3, 1.10);
    let (base, stub) = start_upstream(3, fits, Duration::ZERO).await;
    let cfg = live_cfg(&base, Some(dir.clone()));

    // First client: a /frames-equivalent warm-up writes data + metas.
    let first = IrsaClient::new(cfg.clone()).expect("client");
    first
        .fetch_frames(C_RA, C_DEC, 0.05)
        .await
        .expect("warm fetch");
    let (sia, ibe) = (
        stub.sia_hits.load(Ordering::SeqCst),
        stub.ibe_hits.load(Ordering::SeqCst),
    );
    assert!(sia >= 1 && ibe >= 1);
    drop(first);

    // Simulated restart: new client, empty memory cache, same CACHE_DIR.
    let second = IrsaClient::new(cfg).expect("client");
    let st = live_state(second);
    let (status, v, elapsed) = call(
        &st,
        &format!(
            "/api/v1/spectrum?ra={}&dec={}&aperture_px=3",
            C_RA + 0.004,
            C_DEC + 0.003
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        stub.sia_hits.load(Ordering::SeqCst),
        sia,
        "restart paid an upstream SIA search"
    );
    assert_eq!(
        stub.ibe_hits.load(Ordering::SeqCst),
        ibe,
        "restart paid an upstream cutout fetch"
    );
    assert_eq!(v["points"].as_array().map(Vec::len), Some(3), "{v}");
    assert!(
        elapsed < Duration::from_secs(1),
        "disk-cached spectrum took {elapsed:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// (e) coverage index bound across 1000 requests
// ---------------------------------------------------------------------

/// The secondary coverage index is capped (1024 obs buckets x 64
/// geometries + 512 search cones). 1000 client calls at distinct
/// positions must not grow it past the documented bound for this
/// observation set (3 obs -> 3*64 + 512 = 704 entries).
#[tokio::test]
async fn coverage_index_stays_bounded_across_requests() {
    let fits = cutout_at(C_RA, C_DEC, 8, 0.008, 1.0, 3, 1.10);
    let (base, _stub) = start_upstream(3, fits, Duration::ZERO).await;
    let client = IrsaClient::new(live_cfg(&base, None)).expect("client");

    let mut obs0 = None;
    for i in 0..1000usize {
        // Distinct positions -> distinct cache keys -> index pressure.
        let (ra, dec) = (1.0 + i as f64 * 0.2, (i % 500) as f64 * 0.1 - 25.0);
        if let Ok(obs) = client.search(ra, dec, 0.05).await {
            if let Some(o) = obs.first() {
                obs0 = Some(o.clone());
                let _ = client.fetch_cutout(o, ra, dec, 0.01).await;
            }
        }
    }
    let _ = obs0.expect("stub answered searches");
    // Documented bound: INDEX_MAX_PER_OBS (64) per observation plus
    // INDEX_MAX_SEARCHES (512) cones; 3 distinct observations here.
    let entries = client.coverage_index_len();
    assert!(
        entries <= 3 * 64 + 512,
        "coverage index grew past its bound: {entries} entries"
    );
}

// ---------------------------------------------------------------------
// (f) concurrent identical requests coalesce
// ---------------------------------------------------------------------

/// Eight concurrent identical `/spectrum` requests share one SIA
/// search and one cutout download per observation (the client's
/// `try_get_with` coalescing), not eight of each.
#[tokio::test]
async fn concurrent_identical_spectrum_requests_coalesce() {
    let fits = cutout_at(C_RA, C_DEC, 48, 0.048, 1.0, 3, 1.10);
    let (base, stub) = start_upstream(3, fits, Duration::from_millis(120)).await;
    let st = live_state(IrsaClient::new(live_cfg(&base, None)).expect("client"));

    let uri = format!("/api/v1/spectrum?ra={C_RA}&dec={C_DEC}&aperture_px=3");
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (st, uri) = (st.clone(), uri.clone());
        set.spawn(async move { call(&st, &uri).await });
    }
    let mut oks = 0;
    while let Some(joined) = set.join_next().await {
        let (status, v, _) = joined.expect("request task");
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["points"].as_array().map(Vec::len), Some(3), "{v}");
        oks += 1;
    }
    assert_eq!(oks, 8);
    assert_eq!(
        stub.sia_hits.load(Ordering::SeqCst),
        1,
        "concurrent searches were not coalesced"
    );
    assert_eq!(
        stub.ibe_hits.load(Ordering::SeqCst),
        3,
        "concurrent cutout fetches were not coalesced"
    );
}
