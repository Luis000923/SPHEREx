//! Tests for the SIA resilience work (task b15): the TAP fallback in
//! `IrsaClient::search` and the upstream-reachability fields of
//! `/api/v1/health`.
//!
//! Everything runs against a loopback-only axum stub — no test reaches
//! the real IRSA. The stub answers `/SIA` (SIA2 cone search), `/TAP/sync`
//! (ADQL) and `/ibe/*` (cutouts) with configurable statuses and counts
//! how it was hit.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use serde_json::Value;
use spherex_backend::api::{build_router, AppState};
use spherex_backend::config::Config;
use spherex_backend::error::AppError;
use spherex_backend::irsa::{
    observations_from_tap, tap_cone_query, IrsaClient, IrsaConfig, IrsaError,
};
use tower::ServiceExt;

/// Peer address for `ConnectInfo` (the governor falls back to it).
const TEST_ADDR: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0);

/// The cone the client's SIA probe uses (`client.rs` PROBE_POS).
const PROBE_POS: &str = "circle 0 0 0.0001";

// ---------------------------------------------------------------
// Synthetic VOTable fixtures
// ---------------------------------------------------------------

/// One TAP result row.
struct TapRow<'a> {
    uri: &'a str,
    t_min: f64,
    t_max: f64,
    em_min: f64,
    em_max: f64,
    band: &'a str,
}

/// TAP `/sync` VOTable with the columns `irsa::tap` reads (units as
/// IRSA emits: time bounds in days, energy bounds in metres).
fn tap_votable(rows: &[TapRow]) -> String {
    let mut tr = String::new();
    for r in rows {
        tr.push_str(&format!(
            "<TR><TD>{}</TD><TD>science</TD><TD>{}</TD><TD>{}</TD>\
             <TD>{}</TD><TD>{}</TD><TD>{}</TD></TR>",
            r.uri, r.t_min, r.t_max, r.em_min, r.em_max, r.band
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <VOTABLE version=\"1.3\" xmlns=\"http://www.ivoa.net/xml/VOTable/v1.3\">\
         <RESOURCE type=\"results\"><TABLE>\
         <FIELD name=\"uri\" datatype=\"char\" arraysize=\"*\"/>\
         <FIELD name=\"producttype\" datatype=\"char\" arraysize=\"*\"/>\
         <FIELD name=\"time_bounds_lower\" datatype=\"double\" unit=\"d\"/>\
         <FIELD name=\"time_bounds_upper\" datatype=\"double\" unit=\"d\"/>\
         <FIELD name=\"energy_bounds_lower\" datatype=\"double\" unit=\"m\"/>\
         <FIELD name=\"energy_bounds_upper\" datatype=\"double\" unit=\"m\"/>\
         <FIELD name=\"energy_bandpassname\" datatype=\"char\" arraysize=\"*\"/>\
         <DATA><TABLEDATA>{tr}</TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>"
    )
}

/// Minimal SIA2 VOTable: one data table, `urls.len()` rows.
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

// ---------------------------------------------------------------
// Loopback stub (never the real IRSA)
// ---------------------------------------------------------------

/// Shared stub state: statuses and hit counters per endpoint.
struct Stub {
    /// Status SIA answers with; `sia_body` when it is 200.
    sia_status: Mutex<StatusCode>,
    sia_body: Mutex<String>,
    /// Status TAP answers with; `tap_body` when it is 200.
    tap_status: Mutex<StatusCode>,
    tap_body: String,
    /// All `/SIA` hits (searches and probes).
    sia_hits: AtomicUsize,
    /// `/SIA` hits whose POS is the health-probe cone.
    probe_hits: AtomicUsize,
    /// `/TAP/sync` hits.
    tap_hits: AtomicUsize,
    /// Last ADQL `QUERY` parameter TAP saw (assertion support).
    last_query: Mutex<String>,
}

impl Stub {
    fn sia_ok(body: String) -> Self {
        Self {
            sia_status: Mutex::new(StatusCode::OK),
            sia_body: Mutex::new(body),
            tap_status: Mutex::new(StatusCode::OK),
            tap_body: String::new(),
            sia_hits: AtomicUsize::new(0),
            probe_hits: AtomicUsize::new(0),
            tap_hits: AtomicUsize::new(0),
            last_query: Mutex::new(String::new()),
        }
    }

    fn set_sia(&self, status: StatusCode) {
        *self.sia_status.lock().expect("sia_status") = status;
    }
}

async fn sia_handler(
    State(s): State<Arc<Stub>>,
    Query(q): Query<Vec<(String, String)>>,
) -> Response {
    s.sia_hits.fetch_add(1, Ordering::SeqCst);
    if q.iter().any(|(k, v)| k == "POS" && v == PROBE_POS) {
        s.probe_hits.fetch_add(1, Ordering::SeqCst);
    }
    let status = *s.sia_status.lock().expect("sia_status");
    if status != StatusCode::OK {
        return (status, "stub failure").into_response();
    }
    let body = s.sia_body.lock().expect("sia_body").clone();
    (StatusCode::OK, body).into_response()
}

async fn tap_handler(
    State(s): State<Arc<Stub>>,
    Query(q): Query<Vec<(String, String)>>,
) -> Response {
    s.tap_hits.fetch_add(1, Ordering::SeqCst);
    if let Some((_, v)) = q.iter().find(|(k, _)| k == "QUERY") {
        *s.last_query.lock().expect("last_query") = v.clone();
    }
    let status = *s.tap_status.lock().expect("tap_status");
    if status != StatusCode::OK {
        return (status, "stub failure").into_response();
    }
    (StatusCode::OK, s.tap_body.clone()).into_response()
}

/// Bind a stub on an ephemeral port; returns (base_url, stub state).
async fn start_stub(mut stub: Stub) -> (String, Arc<Stub>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let base = format!("http://{addr}");
    // Fixtures template URLs with "{base}" (the port is only known now).
    stub.tap_body = stub.tap_body.replace("{base}", &base);
    {
        let mut b = stub.sia_body.lock().expect("sia_body");
        *b = b.replace("{base}", &base);
    }
    let stub = Arc::new(stub);
    let app = Router::new()
        .route("/SIA", get(sia_handler))
        .route("/TAP/sync", get(tap_handler))
        .with_state(stub.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, stub)
}

/// Config pointed at the stub: fast, single attempt, short timeouts.
fn test_cfg(base: &str) -> IrsaConfig {
    IrsaConfig {
        base_url: base.to_string(),
        tap_url: format!("{base}/TAP"),
        collections: vec!["spherex_dr1".to_string(), "spherex_qr3".to_string()],
        max_attempts: 1,
        retry_base_delay: Duration::from_millis(1),
        request_timeout: Duration::from_secs(3),
        total_timeout: Duration::from_secs(10),
        health_probe_interval: Duration::from_millis(150),
        ..IrsaConfig::default()
    }
}

/// Poll `f` until true or ~2 s pass (probe tasks are detached).
async fn wait_for(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within 2 s");
}

/// GET `uri` against a router built on `state`, returning (status, json).
async fn get_json<S>(state: AppState<S>, uri: &str) -> (StatusCode, Value)
where
    S: spherex_backend::api::FrameSource,
{
    let app = build_router(state);
    let mut req = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    req.extensions_mut().insert(ConnectInfo(TEST_ADDR));
    let res = app.oneshot(req).await.expect("router answers");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads");
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

// ---------------------------------------------------------------
// Pure TAP parsing (no stub needed)
// ---------------------------------------------------------------

#[test]
fn tap_cone_query_shape() {
    let q = tap_cone_query(133.81891, -7.24705);
    assert!(q.contains("FROM spherex.artifact"));
    assert!(q.contains("JOIN spherex.plane"));
    assert!(q.contains("CONTAINS(POINT('ICRS',133.81891,-7.24705), p.poly)"));
    assert!(q.contains("producttype='science'"));
}

#[test]
fn tap_rows_become_observations() {
    let xml = tap_votable(&[
        TapRow {
            uri: "ibe/data/spherex/dr1/level2/w/f1.fits",
            t_min: 60800.5,
            t_max: 60800.6,
            em_min: 1.63e-6,
            em_max: 2.43e-6,
            band: "SPHEREx-D3",
        },
        // Release not configured -> dropped.
        TapRow {
            uri: "ibe/data/spherex/qr2/level2/w/f2.fits",
            t_min: 60801.0,
            t_max: 60801.1,
            em_min: 4.4e-6,
            em_max: 5.0e-6,
            band: "SPHEREx-D6",
        },
        // Not an IBE artifact path -> dropped.
        TapRow {
            uri: "something/else.fits",
            t_min: 0.0,
            t_max: 0.0,
            em_min: 0.0,
            em_max: 0.0,
            band: "",
        },
        // Absolute access URL -> passed through, still release-mapped.
        TapRow {
            uri: "https://other.example/ibe/data/spherex/qr3/f3.fits",
            t_min: 60802.0,
            t_max: 60802.1,
            em_min: 1.1e-6,
            em_max: 1.2e-6,
            band: "SPHEREx-D1",
        },
    ]);
    let collections = vec!["spherex_dr1".to_string(), "spherex_qr3".to_string()];
    let obs = observations_from_tap(&xml, "https://irsa.test", &collections).expect("parse");
    assert_eq!(obs.len(), 2);
    let dr1 = &obs[0];
    assert_eq!(dr1.collection, "spherex_dr1");
    assert_eq!(
        dr1.access_url,
        "https://irsa.test/ibe/data/spherex/dr1/level2/w/f1.fits"
    );
    assert!((dr1.t_min_mjd - 60800.5).abs() < 1e-9);
    // energy_bounds in metres -> micrometres.
    assert!((dr1.em_min_um - 1.63).abs() < 1e-9);
    assert!((dr1.em_max_um - 2.43).abs() < 1e-9);
    assert_eq!(dr1.band_name, "SPHEREx-D3");
    let qr3 = &obs[1];
    assert_eq!(qr3.collection, "spherex_qr3");
    assert_eq!(
        qr3.access_url,
        "https://other.example/ibe/data/spherex/qr3/f3.fits"
    );
}

#[test]
fn tap_error_votable_is_parse_error() {
    let xml = "<VOTABLE><RESOURCE type=\"results\">\
        <INFO name=\"QUERY_STATUS\" value=\"ERROR\">UsageFault</INFO>\
        </RESOURCE></VOTABLE>";
    let collections = vec!["spherex_qr3".to_string()];
    let e = observations_from_tap(xml, "https://irsa.test", &collections).expect_err("rejects");
    assert!(matches!(e, IrsaError::Parse(_)));
}

// ---------------------------------------------------------------
// Fallback behaviour against the stub
// ---------------------------------------------------------------

/// SIA 502 -> the same cone search is answered by TAP, with identical
/// `Observation` semantics; results are cached like SIA results.
#[tokio::test]
async fn sia_502_falls_back_to_tap() {
    let mut stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_GATEWAY);
    stub.tap_body = tap_votable(&[
        TapRow {
            uri: "ibe/data/spherex/qr3/level2/w/q1.fits",
            t_min: 60810.0,
            t_max: 60810.001,
            em_min: 1.1e-6,
            em_max: 1.2e-6,
            band: "SPHEREx-D1",
        },
        TapRow {
            uri: "ibe/data/spherex/dr1/level2/w/d1.fits",
            t_min: 60809.0,
            t_max: 60809.001,
            em_min: 2.0e-6,
            em_max: 2.1e-6,
            band: "SPHEREx-D4",
        },
        // Duplicate access_url -> deduped.
        TapRow {
            uri: "ibe/data/spherex/qr3/level2/w/q1.fits",
            t_min: 60810.0,
            t_max: 60810.001,
            em_min: 1.1e-6,
            em_max: 1.2e-6,
            band: "SPHEREx-D1",
        },
    ]);
    let (base, stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");

    let obs = client
        .search(10.0, 0.0, 0.05)
        .await
        .expect("fallback answers");
    assert_eq!(obs.len(), 2);
    assert_eq!(obs[0].collection, "spherex_qr3");
    assert_eq!(
        obs[0].access_url,
        format!("{base}/ibe/data/spherex/qr3/level2/w/q1.fits")
    );
    assert_eq!(obs[1].collection, "spherex_dr1");

    // The ADQL cone query actually reached the stub (the second
    // collection's SIA task may be aborted mid-flight once the first
    // fails, so only the lower bound on SIA hits is deterministic).
    assert_eq!(stub.tap_hits.load(Ordering::SeqCst), 1);
    assert!(stub.sia_hits.load(Ordering::SeqCst) >= 1);
    let q = stub.last_query.lock().expect("last_query").clone();
    assert!(q.contains("CONTAINS(POINT('ICRS',10,0)"), "query was: {q}");

    // A repeat search is served from the result cache, not upstream.
    let obs2 = client.search(10.0, 0.0, 0.05).await.expect("cached");
    assert_eq!(obs2.len(), 2);
    assert_eq!(stub.tap_hits.load(Ordering::SeqCst), 1);
}

/// SIA and TAP both down -> typed upstream error that maps to HTTP 502.
#[tokio::test]
async fn sia_and_tap_down_maps_to_502() {
    let stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_GATEWAY);
    *stub.tap_status.lock().expect("tap_status") = StatusCode::BAD_GATEWAY;
    let (base, stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");

    let err = client
        .search(20.0, 5.0, 0.05)
        .await
        .expect_err("both down must fail");
    // The original SIA failure is the reported one.
    assert!(matches!(
        err,
        IrsaError::Http { status: 502 } | IrsaError::Unavailable { .. }
    ));
    let app_err = AppError::from(err);
    assert_eq!(app_err.status_code(), StatusCode::BAD_GATEWAY);
    assert_eq!(app_err.code(), "upstream_unavailable");
    assert!(stub.tap_hits.load(Ordering::SeqCst) >= 1);
}

/// SIA down through the whole HTTP stack: `/frames` answers 502 with
/// the contract error body.
#[tokio::test]
async fn upstream_failure_is_502_through_router() {
    let stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_GATEWAY);
    *stub.tap_status.lock().expect("tap_status") = StatusCode::BAD_GATEWAY;
    let (base, _stub) = start_stub(stub).await;
    let irsa = test_cfg(&base);
    let client = IrsaClient::new(irsa.clone()).expect("client");
    let cfg = Config {
        irsa,
        rate_limit_rps: 1000,
        ..Config::default()
    };
    let state = AppState::from_config(client, &cfg);

    let (status, v) = get_json(state, "/api/v1/frames?ra=10&dec=0&fov=0.05").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(v["error"]["code"], "upstream_unavailable");
}

/// `SIA_FALLBACK_TAP=0`: a dead SIA produces no TAP traffic at all.
#[tokio::test]
async fn fallback_disabled_makes_no_tap_call() {
    let mut stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_GATEWAY);
    stub.tap_body = tap_votable(&[TapRow {
        uri: "ibe/data/spherex/qr3/x.fits",
        t_min: 60810.0,
        t_max: 60810.001,
        em_min: 1.1e-6,
        em_max: 1.2e-6,
        band: "SPHEREx-D1",
    }]);
    let (base, stub) = start_stub(stub).await;
    let mut cfg = test_cfg(&base);
    cfg.sia_fallback_tap = false;
    let client = IrsaClient::new(cfg).expect("client");

    let err = client
        .search(30.0, 1.0, 0.05)
        .await
        .expect_err("fallback off must fail");
    assert!(matches!(err, IrsaError::Http { status: 502 }));
    assert_eq!(stub.tap_hits.load(Ordering::SeqCst), 0);
}

/// Non-5xx SIA failures stay authoritative: an HTTP 400 (a permanent
/// client-side problem upstream) does not trigger the fallback.
#[tokio::test]
async fn sia_4xx_does_not_fall_back() {
    let stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_REQUEST);
    let (base, stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");

    let err = client
        .search(40.0, 2.0, 0.05)
        .await
        .expect_err("4xx propagates");
    assert!(matches!(err, IrsaError::Http { status: 400 }));
    assert_eq!(stub.tap_hits.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------
// /api/v1/health upstream fields
// ---------------------------------------------------------------

/// `upstream_ok` tracks recorded outcomes: optimistic `true` before any
/// traffic, `false` after the probe sees a dead SIA, `true` again after
/// a successful search — and per-channel timestamps appear.
#[tokio::test]
async fn health_reports_upstream_state() {
    let stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_GATEWAY);
    let (base, stub) = start_stub(stub).await;
    let irsa = test_cfg(&base);
    let client = IrsaClient::new(irsa.clone()).expect("client");
    let cfg = Config {
        irsa,
        rate_limit_rps: 1000,
        ..Config::default()
    };
    let state = AppState::from_config(client.clone(), &cfg);

    // Before any outcome the service is optimistic-ok; this call also
    // spawns the first detached probe (SIA is dead -> failure lands).
    let (status, v) = get_json(state.clone(), "/api/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["status"], "ok");
    assert_eq!(v["upstream_ok"], true);

    // Wait for the probe to hit the stub, then the verdict flips.
    let hits = Arc::clone(&stub);
    wait_for(move || hits.probe_hits.load(Ordering::SeqCst) >= 1).await;
    wait_for(|| !client.upstream_status().ok).await;
    let (_, v) = get_json(state.clone(), "/api/v1/health").await;
    assert_eq!(v["upstream_ok"], false);
    assert!(v["upstream"]["last_sia_ok"].is_null());

    // SIA recovers; a real search marks the channel healthy again.
    stub.set_sia(StatusCode::OK);
    *stub.sia_body.lock().expect("sia_body") = sia_votable(&[format!("{base}/ibe/x.fits")]);
    client
        .search(50.0, 3.0, 0.05)
        .await
        .expect("search succeeds");
    let (_, v) = get_json(state, "/api/v1/health").await;
    assert_eq!(v["upstream_ok"], true);
    assert!(v["upstream"]["last_sia_ok"].as_u64().is_some());
}

/// Probes fire at most once per `health_probe_interval`, even under a
/// burst of `/health` requests, and only when idle (no recent contact).
#[tokio::test]
async fn health_probe_is_rate_limited() {
    let stub = Stub::sia_ok(String::new());
    stub.set_sia(StatusCode::BAD_GATEWAY);
    let (base, stub) = start_stub(stub).await;
    let irsa = test_cfg(&base); // health_probe_interval = 150 ms
    let client = IrsaClient::new(irsa.clone()).expect("client");
    let cfg = Config {
        irsa,
        rate_limit_rps: 1000,
        ..Config::default()
    };
    let state = AppState::from_config(client.clone(), &cfg);

    // Burst of health checks: exactly one probe.
    for _ in 0..5 {
        let _ = get_json(state.clone(), "/api/v1/health").await;
    }
    let hits = Arc::clone(&stub);
    wait_for(move || hits.probe_hits.load(Ordering::SeqCst) >= 1).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    for _ in 0..5 {
        let _ = get_json(state.clone(), "/api/v1/health").await;
    }
    assert_eq!(stub.probe_hits.load(Ordering::SeqCst), 1);

    // After the interval passes, the next check probes again — once.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = get_json(state.clone(), "/api/v1/health").await;
    let hits = Arc::clone(&stub);
    wait_for(move || hits.probe_hits.load(Ordering::SeqCst) >= 2).await;
    let _ = get_json(state, "/api/v1/health").await;
    assert_eq!(stub.probe_hits.load(Ordering::SeqCst), 2);
}
