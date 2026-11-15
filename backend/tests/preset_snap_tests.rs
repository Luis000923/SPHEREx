//! Tests for task b20: preset snapping in `api::params` and
//! serve-stale in the disk cache.
//!
//! Snap: a validated `/frames` `(ra, dec, fov)` within
//! `PRESET_SNAP_ARCSEC` of a preset — with `fov` within 1e-3 deg of
//! that preset's — is rewritten to the preset's exact constants before
//! the cache lookup, so re-typed coordinates (sexagesimal strings,
//! rounded decimals, sub-arcsecond re-typings) hit the same cache
//! entries. Verified end to end through the real router + `IrsaClient`
//! against a loopback upstream stub: after the first (exact) request a
//! matching request costs zero upstream calls.
//!
//! Serve-stale: a disk SIA body older than `search_disk_ttl` is
//! refetched; a transient refetch failure serves the stale body instead
//! of erroring (fresh data still wins when upstream answers).
//!
//! No test reaches the real IRSA.

mod fixtures;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use serde_json::Value;
use spherex_backend::api::params::{angular_dist_deg, snap_to_preset, PRESET_SNAP_ARCSEC};
use spherex_backend::api::targets::preset_targets;
use spherex_backend::api::{build_router, AppState};
use spherex_backend::config::Config;
use spherex_backend::irsa::{CutoutSource, IrsaClient, IrsaConfig};
use tower::ServiceExt;

use fixtures::*;

/// Peer address for `ConnectInfo` (the governor falls back to it).
const TEST_ADDR: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0);

/// The five official presets as sexagesimal strings exactly as the
/// frontend sends them (the same table `api::targets::PRESETS` holds);
/// the resolved decimals come from `preset_targets()`.
const PRESETS: &[(&str, &str, &str, f64)] = &[
    ("barnards-star", "17h57m47.102s", "+04d46m05.97s", 0.1),
    ("epsilon-indi-b", "22h04m23.201s", "-56d48m02.34s", 0.1),
    ("wise-0855", "08h55m02.344s", "-07d14m31.86s", 0.1),
    ("rw-aurigae", "05h07m49.581s", "+30d24m04.65s", 0.05),
    ("r-draconis", "16h32m40.136s", "+66d45m18.09s", 0.05),
];

/// Number of synthetic observations the SIA stub lists (all fetched).
const N_OBS: usize = 6;

// ---------------------------------------------------------------------
// Synthetic MEF cutout centred on the requested position
// ---------------------------------------------------------------------

/// Minimal MEF cutout `nx`x`nx` centred on `(ra, dec)` with plate scale
/// `span_deg / nx` — the same shape `support::cutout`/`cutout_at` use.
fn cutout_at(ra: f64, dec: f64, nx: usize, span_deg: f64, wave_um: f32) -> Vec<u8> {
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
        card_int("DETECTOR", 3),
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
        &f32_data(&vec![1.0; nx * ny]),
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

// ---------------------------------------------------------------------
// Loopback upstream stub (never the real IRSA)
// ---------------------------------------------------------------------

/// Minimal SIA2 VOTable with one row per URL (same shape as the b2/b15
/// tests; em bounds in metres, times in MJD days).
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

/// Loopback stub state: per-endpoint status + body and hit counters.
struct UpStub {
    sia_status: Mutex<StatusCode>,
    sia_body: Mutex<String>,
    tap_status: Mutex<StatusCode>,
    /// `/SIA` hits.
    sia_hits: AtomicUsize,
    /// `/ibe/*` hits (cutout downloads).
    ibe_hits: AtomicUsize,
}

impl UpStub {
    fn hits(&self) -> (usize, usize) {
        (
            self.sia_hits.load(Ordering::SeqCst),
            self.ibe_hits.load(Ordering::SeqCst),
        )
    }
}

async fn sia_handler(State(s): State<Arc<UpStub>>) -> Response {
    s.sia_hits.fetch_add(1, Ordering::SeqCst);
    let status = *s.sia_status.lock().expect("sia_status");
    if status != StatusCode::OK {
        return (status, "stub failure").into_response();
    }
    (StatusCode::OK, s.sia_body.lock().expect("sia_body").clone()).into_response()
}

async fn tap_handler(State(s): State<Arc<UpStub>>) -> Response {
    let status = *s.tap_status.lock().expect("tap_status");
    (status, "stub failure").into_response()
}

/// Cutout endpoint: parses `center=ra,dec&size=<n>deg` and answers a
/// synthetic MEF cutout centred exactly on the requested position.
async fn ibe_handler(
    State(s): State<Arc<UpStub>>,
    Query(q): Query<Vec<(String, String)>>,
) -> Response {
    s.ibe_hits.fetch_add(1, Ordering::SeqCst);
    let status = *s.sia_status.lock().expect("sia_status");
    if status != StatusCode::OK {
        return (status, "stub failure").into_response();
    }
    let get = |k: &str| q.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let center = get("center").unwrap_or_else(|| "0,0".to_string());
    let mut it = center.split(',');
    let ra: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let dec: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let span: f64 = get("size")
        .and_then(|v| v.trim_end_matches("deg").parse().ok())
        .unwrap_or(0.05);
    (StatusCode::OK, cutout_at(ra, dec, 29, span, 1.1)).into_response()
}

/// Bind the upstream stub on an ephemeral port; `n_obs` observations
/// listed by the SIA body. Returns (base_url, stub state).
async fn start_upstream(n_obs: usize) -> (String, Arc<UpStub>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("local addr");
    let base = format!("http://{addr}");
    let urls: Vec<String> = (0..n_obs)
        .map(|i| format!("{base}/ibe/f{i}.fits"))
        .collect();
    let stub = Arc::new(UpStub {
        sia_status: Mutex::new(StatusCode::OK),
        sia_body: Mutex::new(sia_votable(&urls)),
        tap_status: Mutex::new(StatusCode::SERVICE_UNAVAILABLE),
        sia_hits: AtomicUsize::new(0),
        ibe_hits: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/SIA", get(sia_handler))
        .route("/TAP/sync", get(tap_handler))
        .route("/ibe/*rest", get(ibe_handler))
        .with_state(Arc::clone(&stub));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, stub)
}

// ---------------------------------------------------------------------
// App wiring
// ---------------------------------------------------------------------

/// Client config pointed at the loopback stub: IBE cutouts only (no S3
/// mirror), single attempt, short timeouts.
fn live_cfg(base: &str, cache_dir: Option<PathBuf>) -> IrsaConfig {
    IrsaConfig {
        base_url: base.to_string(),
        tap_url: format!("{base}/TAP"),
        collections: vec!["spherex_qr3".to_string()],
        cutout_source: CutoutSource::Ibe,
        max_attempts: 1,
        retry_base_delay: Duration::from_millis(1),
        request_timeout: Duration::from_secs(5),
        total_timeout: Duration::from_secs(10),
        cache_dir,
        ..IrsaConfig::default()
    }
}

/// `AppState` over a real `IrsaClient` (the production wiring).
fn live_state(client: &IrsaClient) -> AppState<IrsaClient> {
    let cfg = Config {
        irsa: client.config().clone(),
        rate_limit_rps: 1_000_000,
        ..Config::default()
    };
    AppState::from_config(client.clone(), &cfg)
}

/// GET `uri` through the router; returns (status, json).
async fn get_json(state: &AppState<IrsaClient>, uri: &str) -> (StatusCode, Value) {
    let app = build_router(state.clone());
    let mut req = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    req.extensions_mut().insert(ConnectInfo(TEST_ADDR));
    let res = app.oneshot(req).await.expect("router answers");
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 64 * 1024 * 1024)
        .await
        .expect("body reads");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Percent-encode `+` in a coordinate string for the query string
/// (the frontend sexagesimal declinations carry an explicit sign).
fn enc_coord(s: &str) -> String {
    s.replace('+', "%2B")
}

// ---------------------------------------------------------------------
// Snap: distance function edge cases
// ---------------------------------------------------------------------

/// `angular_dist_deg` is exact on the whole sphere: RA wrap at 0/360
/// and the RA convergence near the poles behave correctly.
#[test]
fn angular_distance_handles_wrap_and_poles() {
    // RA 0/360 wrap: 359.9999 vs 0.0001 deg are ~0.0002 deg apart, not 360.
    let d = angular_dist_deg(359.9999, 0.0, 0.0001, 0.0);
    assert!((d - 0.0002).abs() < 1e-6, "wrap distance was {d}");
    // Near the pole: RA 180 deg apart at dec 89.999 is a few arcseconds,
    // not half a great circle.
    let d = angular_dist_deg(10.0, 89.999, 190.0, 89.999);
    assert!(d < 0.005, "polar distance was {d}");
    // Identical points and a plain mid-latitude separation.
    assert_eq!(angular_dist_deg(269.4, 4.7, 269.4, 4.7), 0.0);
    let d = angular_dist_deg(0.0, 0.0, 0.0, 1.0);
    assert!((d - 1.0).abs() < 1e-12);
}

/// `snap_to_preset`: exact positions snap, 0.05 arcsec snaps back,
/// 0.5 arcsec and fov mismatches do not, non-finite passes through.
#[test]
fn snap_boundaries() {
    let t = &preset_targets().expect("presets")[0]; // barnards-star
    let exact = (t.ra_deg, t.dec_deg, t.fov_deg);
    assert_eq!(snap_to_preset(t.ra_deg, t.dec_deg, t.fov_deg), exact);
    // 0.05 arcsec offsets in each axis snap back to the constants.
    let small = 0.05 / 3600.0;
    assert_eq!(
        snap_to_preset(t.ra_deg, t.dec_deg + small, t.fov_deg),
        exact
    );
    assert_eq!(
        snap_to_preset(t.ra_deg + small, t.dec_deg, t.fov_deg),
        exact
    );
    // 0.5 arcsec does NOT snap.
    let big = 0.5 / 3600.0;
    assert_eq!(
        snap_to_preset(t.ra_deg, t.dec_deg + big, t.fov_deg),
        (t.ra_deg, t.dec_deg + big, t.fov_deg)
    );
    // fov more than 1e-3 deg off does NOT snap; within it does (and
    // reports the preset fov).
    assert_eq!(
        snap_to_preset(t.ra_deg, t.dec_deg, t.fov_deg + 0.002),
        (t.ra_deg, t.dec_deg, t.fov_deg + 0.002)
    );
    assert_eq!(
        snap_to_preset(t.ra_deg, t.dec_deg, t.fov_deg + 0.0005).2,
        t.fov_deg
    );
    // A position far from every preset passes through unchanged.
    assert_eq!(snap_to_preset(10.0, -20.0, 0.05), (10.0, -20.0, 0.05));
    // Non-finite inputs pass through untouched (validation rejects
    // them earlier; the snap must not panic or mangle them).
    assert!(snap_to_preset(f64::NAN, 0.0, 0.05).0.is_nan());
    assert_eq!(snap_to_preset(f64::INFINITY, 0.0, 0.05).0, f64::INFINITY);
}

// ---------------------------------------------------------------------
// Snap: end-to-end through the router + real client
// ---------------------------------------------------------------------

/// For every preset: (a) exact constants, (b) the frontend sexagesimal
/// strings, (c) decimals rounded to 7/6/5/4 places and (d) a 0.05
/// arcsec offset all hit the same cache entries — after the first
/// request the stub sees zero additional upstream calls. A 0.5 arcsec
/// offset does NOT snap and pays the normal upstream cost.
#[tokio::test]
async fn presets_snap_to_same_cache_entries() {
    let (base, stub) = start_upstream(N_OBS).await;
    let client = IrsaClient::new(live_cfg(&base, None)).expect("client");
    let st = live_state(&client);
    let targets = preset_targets().expect("presets");
    assert_eq!(targets.len(), PRESETS.len());

    for &(id, ra_str, dec_str, fov) in PRESETS {
        let t = targets.iter().find(|t| t.id == id).expect("preset row");
        assert_eq!(t.fov_deg, fov, "{id}");

        // (a) exact decimals from /api/v1/targets.
        let (sia0, _) = stub.hits();
        let (s, v) = get_json(
            &st,
            &format!(
                "/api/v1/frames?ra={}&dec={}&fov={}",
                t.ra_deg, t.dec_deg, t.fov_deg
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{id} exact: {v}");
        assert_eq!(v["query"]["ra_deg"].as_f64().unwrap(), t.ra_deg);
        assert_eq!(v["query"]["dec_deg"].as_f64().unwrap(), t.dec_deg);
        assert_eq!(v["query"]["fov_deg"].as_f64().unwrap(), t.fov_deg);
        assert!(
            v["frames"].as_array().is_some_and(|f| !f.is_empty()),
            "{id} exact: no frames"
        );
        // The first request fetched the search + cutouts upstream.
        let (sia1, ibe1) = stub.hits();
        assert!(sia1 > sia0, "{id}: expected a SIA call for a new cone");
        assert!(ibe1 > 0, "{id}: expected cutout fetches");

        // (b) the sexagesimal strings the frontend sends.
        let (s, v) = get_json(
            &st,
            &format!(
                "/api/v1/frames?ra={}&dec={}&fov={}",
                ra_str,
                enc_coord(dec_str),
                t.fov_deg
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{id} sexagesimal: {v}");
        assert_eq!(v["query"]["ra_deg"].as_f64().unwrap(), t.ra_deg);
        assert_eq!(
            stub.hits(),
            (sia1, ibe1),
            "{id}: sexagesimal did not hit the cached entries"
        );

        // (c) decimals rounded to 7, 6, 5 and 4 places.
        for prec in [7usize, 6, 5, 4] {
            let (s, v) = get_json(
                &st,
                &format!(
                    "/api/v1/frames?ra={:.prec$}&dec={:.prec$}&fov={}",
                    t.ra_deg, t.dec_deg, t.fov_deg
                ),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{id} {prec} decimals: {v}");
            // The echo proves the snap: it reports the preset constants.
            assert_eq!(
                v["query"]["ra_deg"].as_f64().unwrap(),
                t.ra_deg,
                "{id} {prec}"
            );
            assert_eq!(
                v["query"]["dec_deg"].as_f64().unwrap(),
                t.dec_deg,
                "{id} {prec}"
            );
            assert_eq!(
                stub.hits(),
                (sia1, ibe1),
                "{id}: {prec}-decimal request missed the cache"
            );
        }

        // (d) 0.05 arcsec offset snaps and hits the same entries.
        let off = 0.05 / 3600.0;
        let (s, v) = get_json(
            &st,
            &format!(
                "/api/v1/frames?ra={}&dec={}&fov={}",
                t.ra_deg,
                t.dec_deg + off,
                t.fov_deg
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{id} 0.05\" offset: {v}");
        assert_eq!(v["query"]["dec_deg"].as_f64().unwrap(), t.dec_deg);
        assert_eq!(
            stub.hits(),
            (sia1, ibe1),
            "{id}: 0.05\" offset missed the cache"
        );

        // (d) 0.5 arcsec offset does NOT snap: the echo reports the
        // requested position and upstream is consulted again.
        let off = 0.5 / 3600.0;
        let want_dec = t.dec_deg + off;
        let (s, v) = get_json(
            &st,
            &format!(
                "/api/v1/frames?ra={}&dec={}&fov={}",
                t.ra_deg, want_dec, t.fov_deg
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{id} 0.5\" offset: {v}");
        assert_eq!(v["query"]["dec_deg"].as_f64().unwrap(), want_dec);
        let (sia2, ibe2) = stub.hits();
        assert!(sia2 > sia1, "{id}: 0.5\" offset should not be cached");
        assert!(ibe2 > ibe1, "{id}: 0.5\" offset should fetch cutouts");
    }
}

/// A request at a preset position but with a different fov does not
/// snap (a different field is a legitimately different cache entry).
#[tokio::test]
async fn fov_mismatch_does_not_snap() {
    let (base, stub) = start_upstream(N_OBS).await;
    let client = IrsaClient::new(live_cfg(&base, None)).expect("client");
    let st = live_state(&client);
    let t = &preset_targets().expect("presets")[0];

    let (sia0, _) = stub.hits();
    let (s, v) = get_json(
        &st,
        &format!("/api/v1/frames?ra={}&dec={}&fov=0.2", t.ra_deg, t.dec_deg),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["query"]["fov_deg"].as_f64().unwrap(), 0.2);
    assert!(stub.hits().0 > sia0, "fov=0.2 should hit upstream");
}

/// NaN/inf coordinates are rejected with 400 before snapping or any
/// upstream traffic.
#[tokio::test]
async fn non_finite_coords_rejected_before_snap() {
    let (base, stub) = start_upstream(N_OBS).await;
    let client = IrsaClient::new(live_cfg(&base, None)).expect("client");
    let st = live_state(&client);
    for (ra, dec) in [
        ("nan", "0"),
        ("NaN", "0"),
        ("inf", "0"),
        ("1e999", "0"),
        ("10", "nan"),
        ("10", "-inf"),
    ] {
        let (s, v) = get_json(&st, &format!("/api/v1/frames?ra={ra}&dec={dec}&fov=0.05")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "ra={ra} dec={dec}: {v}");
        assert_eq!(v["error"]["code"], "invalid_parameter", "{v}");
    }
    assert_eq!(stub.hits(), (0, 0), "rejected input reached upstream");
}

// ---------------------------------------------------------------------
// Serve-stale: aged disk body + transient upstream failure
// ---------------------------------------------------------------------

/// Temp dir for `CACHE_DIR`, removed on drop.
struct TempCache(PathBuf);

impl TempCache {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("spherex-b20-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("mkdir temp cache");
        Self(p)
    }
}

impl Drop for TempCache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Config for the stale tests: tiny disk TTL so entries age out on
/// demand without touching file mtimes.
fn stale_cfg(base: &str, cache_dir: &std::path::Path, ttl: Duration) -> IrsaConfig {
    IrsaConfig {
        search_disk_ttl: ttl,
        ..live_cfg(base, Some(cache_dir.to_path_buf()))
    }
}

/// An aged search body plus a 503 upstream serves the stale entry
/// (HTTP 200 through `/frames` and `/spectrum`); a refetch attempt is
/// still made (the stub counts it).
#[tokio::test]
async fn stale_search_body_served_when_upstream_down() {
    let dir = TempCache::new("stale");
    let ttl = Duration::from_millis(80);
    let (base, stub) = start_upstream(N_OBS).await;
    let cfg = stale_cfg(&base, &dir.0, ttl);

    // Populate the disk cache through the real path.
    let c1 = IrsaClient::new(cfg.clone()).expect("client");
    let st1 = live_state(&c1);
    let t = &preset_targets().expect("presets")[0];
    let frames_uri = format!(
        "/api/v1/frames?ra={}&dec={}&fov={}",
        t.ra_deg, t.dec_deg, t.fov_deg
    );
    let (s, v) = get_json(&st1, &frames_uri).await;
    assert_eq!(s, StatusCode::OK, "warm-up: {v}");
    assert!(dir.0.read_dir().expect("read_dir").count() > 0);

    // Age past the TTL, then break upstream.
    tokio::time::sleep(ttl * 4).await;
    *stub.sia_status.lock().expect("sia_status") = StatusCode::SERVICE_UNAVAILABLE;

    // A fresh client on the same CACHE_DIR (a restart): no memory
    // cache, only the aged disk entries.
    let c2 = IrsaClient::new(cfg.clone()).expect("client");
    let st2 = live_state(&c2);
    let (sia1, _) = stub.hits();
    let (s, v) = get_json(&st2, &frames_uri).await;
    assert_eq!(s, StatusCode::OK, "stale /frames: {v}");
    assert!(
        v["frames"].as_array().is_some_and(|f| !f.is_empty()),
        "stale /frames had no frames: {v}"
    );
    assert!(
        stub.hits().0 > sia1,
        "stale entry was served without a refetch attempt"
    );

    // /spectrum at the same position: the coverage probe rebuilds the
    // observation list from the stale cone body and the cutouts come
    // from disk (no expiry).
    let (s, v) = get_json(
        &st2,
        &format!("/api/v1/spectrum?ra={}&dec={}", t.ra_deg, t.dec_deg),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "stale /spectrum: {v}");
    assert!(
        v["points"].as_array().is_some_and(|p| !p.is_empty()),
        "stale /spectrum had no points: {v}"
    );
}

/// With upstream answering, a stale entry is refreshed: the new body
/// is what gets served and written back.
#[tokio::test]
async fn fresh_data_wins_when_upstream_is_up() {
    let dir = TempCache::new("fresh");
    let ttl = Duration::from_millis(80);
    let (base, stub) = start_upstream(N_OBS).await;
    let cfg = stale_cfg(&base, &dir.0, ttl);

    let c1 = IrsaClient::new(cfg.clone()).expect("client");
    let st1 = live_state(&c1);
    let t = &preset_targets().expect("presets")[0];
    let uri = format!(
        "/api/v1/frames?ra={}&dec={}&fov={}",
        t.ra_deg, t.dec_deg, t.fov_deg
    );
    let (s, v) = get_json(&st1, &uri).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let first_total = v["total_observations"].as_u64().expect("total");

    // Change what upstream reports (one extra observation).
    tokio::time::sleep(ttl * 4).await;
    let urls: Vec<String> = (0..N_OBS + 1)
        .map(|i| format!("{base}/ibe/g{i}.fits"))
        .collect();
    *stub.sia_body.lock().expect("sia_body") = sia_votable(&urls);

    // Fresh client on the same CACHE_DIR: the stale body is refetched.
    let c2 = IrsaClient::new(cfg).expect("client");
    let st2 = live_state(&c2);
    let (sia1, _) = stub.hits();
    let (s, v) = get_json(&st2, &uri).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(stub.hits().0 > sia1, "stale entry was not refetched");
    assert_eq!(
        v["total_observations"].as_u64().expect("total"),
        (N_OBS + 1) as u64,
        "served body was not the refreshed one (first total {first_total})"
    );
}

/// No disk entry + upstream down: the error propagates as 502, no
/// phantom data is invented.
#[tokio::test]
async fn no_disk_entry_and_upstream_down_stays_502() {
    let dir = TempCache::new("empty");
    let (_base, stub) = start_upstream(N_OBS).await;
    // Point the client at the stub but with SIA down (and TAP down too).
    *stub.sia_status.lock().expect("sia_status") = StatusCode::SERVICE_UNAVAILABLE;
    let cfg = stale_cfg(&_base, &dir.0, Duration::from_millis(80));
    let client = IrsaClient::new(cfg).expect("client");
    let st = live_state(&client);
    let t = &preset_targets().expect("presets")[0];
    let (s, v) = get_json(
        &st,
        &format!(
            "/api/v1/frames?ra={}&dec={}&fov={}",
            t.ra_deg, t.dec_deg, t.fov_deg
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["error"]["code"], "upstream_unavailable", "{v}");
}

/// Sanity: the snap radius constant covers the worst 4-decimal-place
/// re-typing of every preset while staying below the 0.5 arcsec
/// non-snap boundary.
#[test]
#[allow(clippy::assertions_on_constants)] // the invariant is constant by design
fn snap_radius_covers_documented_roundings() {
    let worst_theoretical = (2.0f64).sqrt() * 5e-5 * 3600.0; // ~0.255 arcsec
    assert!(
        PRESET_SNAP_ARCSEC > worst_theoretical,
        "{PRESET_SNAP_ARCSEC} does not cover 4-decimal re-typings ({worst_theoretical})"
    );
    assert!(PRESET_SNAP_ARCSEC < 0.5, "0.5\" offsets must not snap");
}
