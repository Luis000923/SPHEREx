//! Tests for the IRSA SIA2 + cutout client (`spherex_backend::irsa`).
//!
//! All fixtures are synthetic: VOTable documents are built as strings
//! and a local axum stub serves them plus synthetic MEF cutouts. No
//! test reaches the real IRSA except `live_irsa_smoke`, which is
//! opt-in via `SPHEREX_LIVE=1`.

mod fixtures;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use fixtures::*;
use spherex_backend::error::AppError;
use spherex_backend::irsa::{
    cutout_url, dedup_by_url, em_to_um, fov_to_cutout_size, observations_from_votable,
    parse_votable, select_frames, IrsaClient, IrsaConfig, IrsaError, Observation, WarmTarget,
};

// ---------------------------------------------------------------
// Synthetic VOTable fixtures
// ---------------------------------------------------------------

/// A minimal SIA2 VOTable: one data table with the columns the client
/// reads, `urls.len()` rows (em in metres, t in MJD days as IRSA emits).
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
         <RESOURCE type=\"meta\"><DESCRIPTION>service metadata</DESCRIPTION></RESOURCE>\
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

/// A valid-but-empty data table (position with no coverage).
const EMPTY_VOTABLE: &str = "<?xml version=\"1.0\"?><VOTABLE><RESOURCE><TABLE>\
    <FIELD name=\"t_min\" datatype=\"double\" unit=\"d\"/>\
    <FIELD name=\"access_url\" datatype=\"char\" arraysize=\"*\"/>\
    <DATA><TABLEDATA></TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>";

/// Minimal SPHEREx MEF cutout (4x4) parseable by `fits::parse_frame`.
fn fake_cutout() -> Vec<u8> {
    let (nx, ny) = (4usize, 4usize);
    let wcs = vec![
        card_str("CTYPE1", "RA---TAN"),
        card_str("CTYPE2", "DEC--TAN"),
        card_flt("CRPIX1", 2.5),
        card_flt("CRPIX2", 2.5),
        card_flt("CRVAL1", 337.878),
        card_flt("CRVAL2", -5.189),
        card_flt("CD1_1", -0.001),
        card_flt("CD1_2", 0.0),
        card_flt("CD2_1", 0.0),
        card_flt("CD2_2", 0.001),
        card_int("DETECTOR", 3),
        card_str("BUNIT", "MJy / sr"),
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
        &[],
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
        &[1, 4],
        &[1, 4],
        &[1.10, 0.05, 1.20, 0.05, 1.15, 0.05, 1.25, 0.05],
        &[],
    ));
    f
}

// ---------------------------------------------------------------
// Local axum stub (never the real IRSA)
// ---------------------------------------------------------------

/// Shared stub state: counts hits, fails the first `sia_fail_first`
/// SIA requests with 503, measures in-flight cutout requests.
struct Stub {
    sia_hits: AtomicUsize,
    sia_fail_first: usize,
    sia_body: String,
    ibe_hits: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    ibe_delay: Duration,
    /// When set, every cutout request answers with this status instead
    /// of the FITS body (503 = busy, 422 = arrays do not overlap).
    ibe_status: Option<StatusCode>,
    fits: Vec<u8>,
}

async fn sia_handler(State(s): State<Arc<Stub>>) -> Response {
    let n = s.sia_hits.fetch_add(1, Ordering::SeqCst) + 1;
    if n <= s.sia_fail_first {
        return (StatusCode::SERVICE_UNAVAILABLE, "busy").into_response();
    }
    (StatusCode::OK, s.sia_body.clone()).into_response()
}

async fn ibe_handler(State(s): State<Arc<Stub>>) -> Response {
    s.ibe_hits.fetch_add(1, Ordering::SeqCst);
    let cur = s.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    s.max_in_flight.fetch_max(cur, Ordering::SeqCst);
    tokio::time::sleep(s.ibe_delay).await;
    s.in_flight.fetch_sub(1, Ordering::SeqCst);
    if let Some(status) = s.ibe_status {
        return (status, "stub failure").into_response();
    }
    (StatusCode::OK, s.fits.clone()).into_response()
}

/// Bind a stub on an ephemeral port; returns (base_url, stub state).
async fn start_stub(mut stub: Stub) -> (String, Arc<Stub>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let base = format!("http://{addr}");
    if stub.sia_body.is_empty() {
        let urls: Vec<String> = (0..3).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect();
        stub.sia_body = sia_votable(&urls);
    }
    // Fixtures template access_url with the literal "{base}" so they can
    // be built before the ephemeral port is known.
    stub.sia_body = stub.sia_body.replace("{base}", &base);
    let stub = Arc::new(stub);
    let app = Router::new()
        .route("/SIA", get(sia_handler))
        .route("/ibe/*rest", get(ibe_handler))
        .with_state(stub.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, stub)
}

fn stub_with_urls(urls: Vec<String>, fits: Vec<u8>) -> Stub {
    Stub {
        sia_hits: AtomicUsize::new(0),
        sia_fail_first: 0,
        sia_body: sia_votable(&urls),
        ibe_hits: AtomicUsize::new(0),
        in_flight: AtomicUsize::new(0),
        max_in_flight: AtomicUsize::new(0),
        ibe_delay: Duration::from_millis(0),
        ibe_status: None,
        fits,
    }
}

/// Test config pointed at the stub, with fast retries.
fn test_cfg(base: &str) -> IrsaConfig {
    IrsaConfig {
        base_url: base.to_string(),
        collections: vec!["spherex_qr3".to_string()],
        retry_base_delay: Duration::from_millis(1),
        request_timeout: Duration::from_secs(5),
        ..IrsaConfig::default()
    }
}

fn obs_with_url(url: &str, t_mjd: f64) -> Observation {
    Observation {
        t_min_mjd: t_mjd,
        t_max_mjd: t_mjd + 0.001,
        em_min_um: 1.0,
        em_max_um: 1.2,
        band_name: "SPHEREx-D1".to_string(),
        access_url: url.to_string(),
        collection: "spherex_qr3".to_string(),
    }
}

// ---------------------------------------------------------------
// Pure parsing / selection tests
// ---------------------------------------------------------------

#[test]
fn votable_parses_table_with_entities() {
    let xml = "<?xml version=\"1.0\"?><!DOCTYPE VOTABLE [ <!ENTITY x \"y\"> ]>\
        <VOTABLE><RESOURCE><TABLE>\
        <FIELD name=\"a\" datatype=\"double\" unit=\"m\"/>\
        <FIELD name=\"b\" datatype=\"char\" arraysize=\"*\"/>\
        <DATA><TABLEDATA>\
        <TR><TD>1.5</TD><TD>x&amp;y&quot;z</TD></TR>\
        <TR><TD/><TD><![CDATA[c<d>]]></TD></TR>\
        </TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>";
    let tables = parse_votable(xml).expect("parse");
    assert_eq!(tables.len(), 1);
    let t = &tables[0];
    assert_eq!(t.fields.len(), 2);
    assert_eq!(t.fields[0].name, "a");
    assert_eq!(t.fields[0].unit.as_deref(), Some("m"));
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0][1], "x&y\"z");
    assert_eq!(t.rows[1][0], "");
    assert_eq!(t.rows[1][1], "c<d>");
}

#[test]
fn sia_rows_become_observations_with_units() {
    let urls = vec!["https://irsa.ipac.caltech.edu/ibe/x.fits".to_string()];
    let obs = observations_from_votable(&sia_votable(&urls), "spherex_qr3").expect("obs");
    assert_eq!(obs.len(), 1);
    let o = &obs[0];
    assert!((o.t_min_mjd - 61000.0).abs() < 1e-9);
    // em was declared in metres -> micrometres.
    assert!((o.em_min_um - 1.10).abs() < 1e-9);
    assert!((o.em_max_um - 1.117).abs() < 1e-9);
    assert_eq!(o.band_name, "SPHEREx-D1");
    assert_eq!(o.collection, "spherex_qr3");
    assert_eq!(o.id().len(), 16);
}

#[test]
fn sia_skips_rows_without_access_url() {
    let xml = "<VOTABLE><RESOURCE><TABLE>\
        <FIELD name=\"t_min\" datatype=\"double\" unit=\"d\"/>\
        <FIELD name=\"access_url\" datatype=\"char\" arraysize=\"*\"/>\
        <DATA><TABLEDATA>\
        <TR><TD>1.0</TD><TD/></TR>\
        <TR><TD>2.0</TD><TD>http://x/f.fits</TD></TR>\
        </TABLEDATA></DATA></TABLE></RESOURCE></VOTABLE>";
    let obs = observations_from_votable(xml, "c").expect("obs");
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].access_url, "http://x/f.fits");
}

#[test]
fn em_unit_conversion() {
    assert!((em_to_um(1.117e-6, Some("m")) - 1.117).abs() < 1e-9);
    assert!((em_to_um(1.117, Some("um")) - 1.117).abs() < 1e-9);
    assert!((em_to_um(1117.0, Some("nm")) - 1.117).abs() < 1e-6);
    // Unknown/absent unit: sub-millimetre values read as metres.
    assert!((em_to_um(1.117e-6, None) - 1.117).abs() < 1e-9);
    assert!((em_to_um(1.117, None) - 1.117).abs() < 1e-9);
}

#[test]
fn dedup_keeps_first_collection_hit() {
    let a = obs_with_url("http://x/same.fits", 1.0);
    let mut b = obs_with_url("http://x/same.fits", 1.0);
    b.collection = "spherex_dr1".to_string();
    let c = obs_with_url("http://x/other.fits", 2.0);
    let out = dedup_by_url(vec![a, b, c]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].collection, "spherex_qr3");
}

#[test]
fn fov_maps_to_square_degrees() {
    // IBE cutouts: size defaults to degrees, one value = square
    // (https://irsa.ipac.caltech.edu/ibe/cutouts.html).
    assert_eq!(fov_to_cutout_size(0.05), "0.05deg");
    let url = cutout_url("https://h/x.fits", 337.8783221, -5.1891588, 0.05);
    assert_eq!(
        url,
        "https://h/x.fits?center=337.8783221,-5.1891588&size=0.05deg"
    );
    let url2 = cutout_url("https://h/x.fits?raw=1", 1.0, 2.0, 0.01);
    assert!(url2.contains("&center=1,2&size=0.01deg"));
}

#[test]
fn select_frames_spreads_over_time_newest_first() {
    let obs: Vec<Observation> = (0..100)
        .map(|i| obs_with_url(&format!("{{base}}/ibe/f{i}.fits"), 61000.0 + i as f64))
        .collect();
    let picked = select_frames(&obs, 10);
    assert_eq!(picked.len(), 10);
    // Evenly spaced indices 0,11,22,...,99 -> epochs 99,88,...,0 desc.
    assert!((picked[0].t_min_mjd - 61099.0).abs() < 1e-9);
    assert!((picked[9].t_min_mjd - 61000.0).abs() < 1e-9);
    for w in picked.windows(2) {
        assert!(w[0].t_min_mjd > w[1].t_min_mjd);
    }
    // Below the cap: all kept, still newest first.
    let all = select_frames(&obs[..5], 10);
    assert_eq!(all.len(), 5);
    assert!((all[0].t_min_mjd - 61004.0).abs() < 1e-9);
}

// ---------------------------------------------------------------
// HTTP client tests against the local stub
// ---------------------------------------------------------------

#[tokio::test]
async fn search_retries_on_503_then_succeeds() {
    let mut stub = stub_with_urls(vec!["{base}/ibe/f0.fits".to_string()], fake_cutout());
    stub.sia_fail_first = 2; // 503, 503, then 200
    let (base, stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let obs = client.search(337.8783221, -5.1891588, 0.05).await;
    assert!(obs.is_ok(), "expected success after retries: {obs:?}");
    assert_eq!(stub.sia_hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn unavailable_after_retries_maps_to_502() {
    let mut stub = stub_with_urls(vec![], fake_cutout());
    stub.sia_fail_first = usize::MAX;
    let (base, _stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let err = client
        .search(337.8783221, -5.1891588, 0.05)
        .await
        .expect_err("must fail");
    assert!(matches!(err, IrsaError::Unavailable { attempts: 3, .. }));
    let app: AppError = err.into();
    assert_eq!(app.status_code(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn empty_result_maps_to_404() {
    let mut stub = stub_with_urls(vec![], fake_cutout());
    stub.sia_body = EMPTY_VOTABLE.to_string();
    let (base, _stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let err = client
        .search(337.8783221, -5.1891588, 0.05)
        .await
        .expect_err("must fail");
    assert!(matches!(err, IrsaError::NoObservations));
    let app: AppError = err.into();
    assert_eq!(app.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dedup_across_collections_over_http() {
    // Two configured collections returning the same access_url merge
    // into a single observation.
    let (base, _s) = {
        let stub = stub_with_urls(vec!["{base}/ibe/shared.fits".to_string()], fake_cutout());
        start_stub(stub).await
    };
    let mut cfg = test_cfg(&base);
    cfg.collections = vec!["spherex_dr1".to_string(), "spherex_qr3".to_string()];
    let client = IrsaClient::new(cfg).expect("client");
    let obs = client.search(10.0, -5.0, 0.05).await.expect("search");
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].collection, "spherex_dr1"); // first wins
}

#[tokio::test]
async fn fetch_frames_decodes_cutouts() {
    let (base, _stub) = start_stub(stub_with_urls(
        (0..3).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect(),
        fake_cutout(),
    ))
    .await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let set = client
        .fetch_frames(337.8783221, -5.1891588, 0.05)
        .await
        .expect("frames");
    assert_eq!(set.frames.len(), 3);
    assert_eq!(set.total_observations, 3);
    assert!(!set.truncated);
    let f = &set.frames[0].frame;
    assert_eq!(f.header.detector, Some(3));
    assert_eq!(f.image.dim(), (4, 4));
    // Newest first.
    assert!(set.frames[0].observation.t_min_mjd > set.frames[2].observation.t_min_mjd);
}

#[tokio::test]
async fn concurrency_is_bounded_by_semaphore() {
    let urls: Vec<String> = (0..8).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect();
    let mut stub = stub_with_urls(urls, fake_cutout());
    stub.ibe_delay = Duration::from_millis(60);
    let (base, stub) = start_stub(stub).await;
    let mut cfg = test_cfg(&base);
    cfg.max_concurrency = 2;
    cfg.max_frames = 8;
    let client = IrsaClient::new(cfg).expect("client");
    let set = client
        .fetch_frames(337.8783221, -5.1891588, 0.05)
        .await
        .expect("frames");
    assert_eq!(set.frames.len(), 8);
    assert_eq!(stub.ibe_hits.load(Ordering::SeqCst), 8);
    let peak = stub.max_in_flight.load(Ordering::SeqCst);
    assert!(peak <= 2, "semaphore bound violated: peak {peak}");
    assert!(peak >= 2, "expected parallelism to be used: peak {peak}");
}

#[tokio::test]
async fn body_size_cap_rejects_large_responses() {
    let (base, _stub) = start_stub(stub_with_urls(
        vec!["{base}/ibe/f0.fits".to_string()],
        fake_cutout(),
    ))
    .await;
    let mut cfg = test_cfg(&base);
    cfg.max_body_bytes = 4096; // smaller than the synthetic cutout
    let client = IrsaClient::new(cfg).expect("client");
    let obs = obs_with_url(&format!("{base}/ibe/big.fits"), 61000.0);
    let err = client
        .fetch_cutout(&obs, 337.8783221, -5.1891588, 0.05)
        .await
        .expect_err("must fail");
    assert!(matches!(err, IrsaError::BodyTooLarge { .. }), "got {err:?}");
}

#[tokio::test]
async fn cache_avoids_refetching() {
    let (base, stub) = start_stub(stub_with_urls(
        (0..3).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect(),
        fake_cutout(),
    ))
    .await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    for _ in 0..2 {
        let set = client
            .fetch_frames(337.8783221, -5.1891588, 0.05)
            .await
            .expect("frames");
        assert_eq!(set.frames.len(), 3);
    }
    assert_eq!(stub.sia_hits.load(Ordering::SeqCst), 1, "search not cached");
    assert_eq!(
        stub.ibe_hits.load(Ordering::SeqCst),
        3,
        "cutouts not cached"
    );
}

/// Regression (integration-live): a cutout that failed upstream was
/// re-requested on every API call, so a "cached" `/frames` request
/// still spent ~30 s retrying the one bad observation. Failures are now
/// remembered for a short TTL.
#[tokio::test]
async fn failed_cutout_is_not_refetched_immediately() {
    for status in [
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::UNPROCESSABLE_ENTITY,
    ] {
        let mut stub = stub_with_urls(vec![], fake_cutout());
        stub.ibe_status = Some(status);
        let (base, stub) = start_stub(stub).await;
        let cfg = test_cfg(&base);
        let attempts = cfg.max_attempts;
        let client = IrsaClient::new(cfg).expect("client");
        let obs = obs_with_url(&format!("{base}/ibe/bad.fits"), 61000.0);
        for _ in 0..3 {
            let err = client
                .fetch_cutout(&obs, 337.8783221, -5.1891588, 0.05)
                .await
                .expect_err("upstream always fails");
            let app: AppError = err.into();
            assert!(
                matches!(app, AppError::UpstreamUnavailable | AppError::Internal),
                "{status}: unexpected mapping {app:?}"
            );
        }
        let expected = if status == StatusCode::SERVICE_UNAVAILABLE {
            attempts
        } else {
            1
        };
        assert_eq!(
            stub.ibe_hits.load(Ordering::SeqCst),
            expected,
            "{status}: failed cutout re-requested instead of negative-cached"
        );
    }
}

/// Regression (integration-live): two API requests for the same target
/// in flight at once downloaded every cutout twice. Concurrent
/// identical fetches now share one upstream request.
#[tokio::test]
async fn concurrent_identical_cutouts_are_coalesced() {
    let mut stub = stub_with_urls(vec![], fake_cutout());
    stub.ibe_delay = Duration::from_millis(150);
    let (base, stub) = start_stub(stub).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let obs = obs_with_url(&format!("{base}/ibe/same.fits"), 61000.0);
    let (a, b, c) = tokio::join!(
        client.fetch_cutout(&obs, 337.8783221, -5.1891588, 0.05),
        client.fetch_cutout(&obs, 337.8783221, -5.1891588, 0.05),
        client.fetch_cutout(&obs, 337.8783221, -5.1891588, 0.05),
    );
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(
        stub.ibe_hits.load(Ordering::SeqCst),
        1,
        "fetch not coalesced"
    );
}

/// integration-live: a real cold cutout costs ~1 min upstream (3.96 MB
/// at ~60 KB/s), so losing the memory cache on every restart/deploy is
/// expensive. With `CACHE_DIR` set, a new client (= restarted process)
/// serves searches and cutouts from disk without touching upstream.
#[tokio::test]
async fn disk_cache_survives_restart() {
    let dir = std::env::temp_dir().join(format!(
        "spherex-disk-cache-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let (base, stub) = start_stub(stub_with_urls(
        (0..2).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect(),
        fake_cutout(),
    ))
    .await;
    let cfg = IrsaConfig {
        cache_dir: Some(dir.clone()),
        ..test_cfg(&base)
    };
    let first = IrsaClient::new(cfg.clone())
        .expect("client")
        .fetch_frames(337.8783221, -5.1891588, 0.05)
        .await
        .expect("frames");
    assert_eq!(first.frames.len(), 2);
    let (sia, ibe) = (
        stub.sia_hits.load(Ordering::SeqCst),
        stub.ibe_hits.load(Ordering::SeqCst),
    );
    let second = IrsaClient::new(cfg)
        .expect("client")
        .fetch_frames(337.8783221, -5.1891588, 0.05)
        .await
        .expect("frames from disk");
    assert_eq!(second.frames.len(), 2);
    assert_eq!(second.frames[0].frame.image, first.frames[0].frame.image);
    assert_eq!(
        stub.sia_hits.load(Ordering::SeqCst),
        sia,
        "search not on disk"
    );
    assert_eq!(
        stub.ibe_hits.load(Ordering::SeqCst),
        ibe,
        "cutouts not on disk"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression (integration-live): IRSA streamed real 3.96 MB cutouts
/// at ~60 KB/s (66 s each). The 15/30 s *total* request timeout cut every
/// one of them mid-body ("error decoding response body"), so nothing
/// ever decoded. `IRSA_TIMEOUT_SECS` is now an idle-read timeout: a
/// slow body that keeps arriving completes.
#[tokio::test]
async fn slow_but_steady_body_is_not_cut_by_timeout() {
    async fn slow_body() -> Response {
        let fits = fake_cutout();
        let chunks: Vec<Vec<u8>> = fits
            .chunks(fits.len().div_ceil(6))
            .map(<[u8]>::to_vec)
            .collect();
        let stream = futures_util::stream::unfold(chunks.into_iter(), |mut it| async move {
            let next = it.next()?;
            tokio::time::sleep(Duration::from_millis(120)).await;
            Some((Ok::<_, std::io::Error>(next), it))
        });
        axum::body::Body::from_stream(stream).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let app = Router::new().route("/ibe/*rest", get(slow_body));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let cfg = IrsaConfig {
        // Whole body takes ~720 ms; no single gap exceeds ~120 ms.
        request_timeout: Duration::from_millis(400),
        max_attempts: 1,
        ..test_cfg(&base)
    };
    let client = IrsaClient::new(cfg).expect("client");
    let obs = obs_with_url(&format!("{base}/ibe/slow.fits"), 61000.0);
    let frame = client
        .fetch_cutout(&obs, 337.8783221, -5.1891588, 0.05)
        .await
        .expect("slow but steady download must complete");
    assert_eq!(frame.header.detector, Some(3));
}

/// Synthetic full-frame parent (nx x ny): IMAGE = x + 1000 y,
/// FLAGS = RICE row-tiles of (7x + y) % 5, constant VARIANCE/ZODI, a
/// large EPSF that must never be downloaded, and WCS-WAVE.
fn fake_parent(nx: usize, ny: usize) -> Vec<u8> {
    let wcs = vec![
        card_str("CTYPE1", "RA---TAN"),
        card_str("CTYPE2", "DEC--TAN"),
        card_flt("CRPIX1", nx as f64 / 2.0 + 0.5),
        card_flt("CRPIX2", ny as f64 / 2.0 + 0.5),
        card_flt("CRVAL1", 10.0),
        card_flt("CRVAL2", 0.0),
        card_flt("CD1_1", -0.001),
        card_flt("CD1_2", 0.0),
        card_flt("CD2_1", 0.0),
        card_flt("CD2_2", 0.001),
        card_flt("CRPIX1A", 1.0),
        card_flt("CRPIX2A", 1.0),
        card_int("DETECTOR", 4),
        card_str("BUNIT", "MJy / sr"),
    ];
    let image: Vec<f32> = (0..nx * ny)
        .map(|i| (i % nx) as f32 + 1000.0 * (i / nx) as f32)
        .collect();
    let flags: Vec<i32> = (0..nx * ny)
        .map(|i| ((7 * (i % nx) + i / nx) % 5) as i32)
        .collect();
    let mut legend = wcs.clone();
    legend.push(card_str("MSKN0000", "TRANSIENT"));
    legend.push(card_str("MSKN0002", "SUR_ERROR"));
    let mut f = primary_hdu();
    f.extend(image_hdu("IMAGE", -32, nx, ny, &f32_data(&image), &wcs));
    f.extend(compressed_image_hdu(
        "FLAGS",
        [nx, ny],
        [nx, 1],
        &flags,
        &[],
        &legend,
    ));
    f.extend(image_hdu(
        "VARIANCE",
        -32,
        nx,
        ny,
        &f32_data(&vec![0.5; nx * ny]),
        &wcs,
    ));
    f.extend(image_hdu(
        "ZODI",
        -32,
        nx,
        ny,
        &f32_data(&vec![0.1; nx * ny]),
        &wcs,
    ));
    f.extend(image_hdu(
        "EPSF",
        -32,
        256,
        256,
        &f32_data(&vec![9.0; 256 * 256]),
        &[],
    ));
    f.extend(wcs_wave_hdu(
        &[1, nx as i32],
        &[1, ny as i32],
        &[1.10, 0.05, 1.20, 0.05, 1.15, 0.05, 1.25, 0.05],
        &[],
    ));
    f
}

struct RangeStub {
    file: Vec<u8>,
    bytes_served: AtomicUsize,
    requests: AtomicUsize,
}

async fn range_handler(
    State(s): State<Arc<RangeStub>>,
    headers: axum::http::HeaderMap,
) -> Response {
    s.requests.fetch_add(1, Ordering::SeqCst);
    let spec = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .unwrap_or_default()
        .to_string();
    if let Some(n) = spec.strip_prefix('-').and_then(|n| n.parse::<usize>().ok()) {
        let start = s.file.len().saturating_sub(n);
        s.bytes_served
            .fetch_add(s.file.len() - start, Ordering::SeqCst);
        return (StatusCode::PARTIAL_CONTENT, s.file[start..].to_vec()).into_response();
    }
    let Some(r) = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
    else {
        return (StatusCode::OK, s.file.clone()).into_response();
    };
    if r.0 >= s.file.len() {
        return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    }
    let end = (r.1 + 1).min(s.file.len());
    s.bytes_served.fetch_add(end - r.0, Ordering::SeqCst);
    (StatusCode::PARTIAL_CONTENT, s.file[r.0..end].to_vec()).into_response()
}

/// integration-live: cutouts are rebuilt from S3 byte ranges of the
/// full-frame file (IBE served 4 MB at ~55 KB/s). The result must equal
/// the exact sub-array the cutout service would return, FLAGS decoded
/// from only the needed row-tiles, with the WCS shifted, without
/// downloading EPSF and without touching IBE.
#[tokio::test]
async fn s3_range_cutout_matches_subarray() {
    let (nx, ny) = (64usize, 48usize);
    let file = fake_parent(nx, ny);
    let rs = Arc::new(RangeStub {
        file: file.clone(),
        bytes_served: AtomicUsize::new(0),
        requests: AtomicUsize::new(0),
    });
    let (base, stub) = start_stub(stub_with_urls(vec![], fake_cutout())).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let s3 = format!("http://{}", listener.local_addr().expect("addr"));
    let app = Router::new()
        .route("/*rest", get(range_handler))
        .with_state(rs.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let cfg = IrsaConfig {
        s3_base_url: s3,
        ..test_cfg(&base)
    };
    let client = IrsaClient::new(cfg).expect("client");
    let obs = obs_with_url(
        &format!("{base}/ibe/data/spherex/dr1/level2/x/parent.fits"),
        61000.0,
    );
    // 0.011 deg / 0.001 deg/px -> 11 px box centred on CRVAL.
    let frame = client
        .fetch_cutout(&obs, 10.0, 0.0, 0.011)
        .await
        .expect("cutout from ranges");
    assert_eq!(stub.ibe_hits.load(Ordering::SeqCst), 0, "fell back to IBE");
    assert_eq!(frame.image.dim(), (11, 11));
    // Centre pixel 32.5 rounds to 33 (1-based) -> first column 28 -> x0 = 27;
    // rows: 24.5 -> 25 -> first row 20 -> y0 = 19.
    let (x0, y0) = (27usize, 19usize);
    for y in 0..11 {
        for x in 0..11 {
            let (px, py) = (x0 + x, y0 + y);
            assert_eq!(frame.image[[y, x]], px as f32 + 1000.0 * py as f32);
            assert_eq!(
                frame.flags[[y, x]],
                ((7 * px + py) % 5) as i32,
                "flags at {x},{y}"
            );
        }
    }
    // The sky position still maps to the same sky pixel after the shift.
    let (cx, cy) = frame.wcs.radec_to_pixel(10.0, 0.0).expect("inside");
    assert!((cx - (32.5 - x0 as f64)).abs() < 1e-9 && (cy - (24.5 - y0 as f64)).abs() < 1e-9);
    assert!(!frame.header.has_epsf);
    assert_eq!(frame.header.detector, Some(4));
    assert!(frame
        .header
        .flag_legend
        .values()
        .any(|b| b.name == "SUR_ERROR"));
    let served = rs.bytes_served.load(Ordering::SeqCst);
    assert!(
        served < file.len() / 2,
        "downloaded {served} of {} bytes",
        file.len()
    );

    // A box outside the array is a 422, like the cutout service.
    let err = client
        .fetch_cutout(&obs, 20.0, 0.0, 0.011)
        .await
        .expect_err("no overlap");
    assert!(matches!(err, IrsaError::Http { status: 422 }), "{err:?}");
    assert_eq!(stub.ibe_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn max_frames_caps_and_reports_truncation() {
    let urls: Vec<String> = (0..6).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect();
    let (base, _stub) = start_stub(stub_with_urls(urls, fake_cutout())).await;
    let mut cfg = test_cfg(&base);
    cfg.max_frames = 3;
    let client = IrsaClient::new(cfg).expect("client");
    let set = client
        .fetch_frames(337.8783221, -5.1891588, 0.05)
        .await
        .expect("frames");
    assert_eq!(set.frames.len(), 3);
    assert_eq!(set.total_observations, 6);
    assert!(set.truncated);
}

#[tokio::test]
async fn warm_prefetches_targets() {
    let (base, _stub) = start_stub(stub_with_urls(
        (0..2).map(|i| format!("{{base}}/ibe/f{i}.fits")).collect(),
        fake_cutout(),
    ))
    .await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let report = client
        .warm(&[WarmTarget {
            ra_deg: 337.8783221,
            dec_deg: -5.1891588,
            fov_deg: 0.05,
        }])
        .await;
    assert_eq!(report.succeeded, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.frames, 2);
}

#[tokio::test]
async fn invalid_coordinates_rejected_before_http() {
    let (base, stub) = start_stub(stub_with_urls(vec![], fake_cutout())).await;
    let client = IrsaClient::new(test_cfg(&base)).expect("client");
    let err = client
        .fetch_frames(400.0, -5.0, 0.05)
        .await
        .expect_err("must fail");
    assert!(matches!(err, IrsaError::InvalidQuery(_)));
    let app: AppError = err.into();
    assert_eq!(app.status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(stub.sia_hits.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------
// Opt-in live smoke test (never runs by default)
// ---------------------------------------------------------------

/// Real end-to-end check against IRSA. Enable with `SPHEREX_LIVE=1`.
#[tokio::test]
async fn live_irsa_smoke() {
    if std::env::var("SPHEREX_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let client = IrsaClient::from_env().expect("client from env");
    let obs = client
        .search(337.8783221, -5.1891588, 0.05)
        .await
        .expect("live search");
    assert!(!obs.is_empty());
    // Some SIA-listed files legitimately fail per-cutout (422 "Arrays
    // do not overlap" when the square misses the array, transient
    // 503s under load). Probe a few; at least one must decode.
    let mut decoded = 0usize;
    for o in obs.iter().take(5) {
        match client.fetch_cutout(o, 337.8783221, -5.1891588, 0.05).await {
            Ok(f) => {
                let det = f.header.detector.expect("DETECTOR header");
                assert!((1..=6).contains(&det));
                decoded += 1;
            }
            Err(e) => eprintln!("live cutout failed (tolerated): {e}"),
        }
    }
    assert!(decoded >= 1, "no live cutout decoded");
}

/// Opt-in live check (`SPHEREX_LIVE=1` plus a reference cutout already
/// downloaded from IBE): the S3-range cutout must equal the IBE cutout
/// pixel for pixel. Env: `SPHEREX_IBE_REF` (path to the IBE FITS),
/// `SPHEREX_REF_URL` (its access_url), `SPHEREX_REF_POS` ("ra,dec,size").
#[tokio::test]
async fn live_s3_cutout_matches_ibe_reference() {
    if std::env::var("SPHEREX_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let (Ok(path), Ok(url), Ok(pos)) = (
        std::env::var("SPHEREX_IBE_REF"),
        std::env::var("SPHEREX_REF_URL"),
        std::env::var("SPHEREX_REF_POS"),
    ) else {
        eprintln!("skipping: SPHEREX_IBE_REF / SPHEREX_REF_URL / SPHEREX_REF_POS not set");
        return;
    };
    let p: Vec<f64> = pos
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    assert_eq!(p.len(), 3, "SPHEREX_REF_POS must be ra,dec,size");
    let reference = spherex_backend::fits::parse_frame(&std::fs::read(&path).expect("read ref"))
        .expect("parse ref");
    let cfg = IrsaConfig {
        // Never fall back to IBE here: the point is the mirror path.
        base_url: "http://127.0.0.1:9".into(),
        max_attempts: 2,
        ..IrsaConfig::default()
    };
    let client = IrsaClient::new(cfg).expect("client");
    let obs = obs_with_url(&url, 61000.0);
    let t0 = std::time::Instant::now();
    let got = client
        .fetch_cutout(&obs, p[0], p[1], p[2])
        .await
        .expect("S3 cutout");
    eprintln!("S3 cutout {:?} in {:?}", got.image.dim(), t0.elapsed());
    assert_eq!(got.image.dim(), reference.image.dim());
    let same = |a: &ndarray::Array2<f32>, b: &ndarray::Array2<f32>| {
        a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.to_bits() == y.to_bits())
    };
    assert!(same(&got.image, &reference.image), "IMAGE differs");
    assert!(same(&got.variance, &reference.variance), "VARIANCE differs");
    assert!(same(&got.zodi, &reference.zodi), "ZODI differs");
    assert_eq!(got.flags, reference.flags, "FLAGS differ");
    assert_eq!(got.wcs.crpix, reference.wcs.crpix);
    assert_eq!(
        got.header.flag_legend.len(),
        reference.header.flag_legend.len()
    );
    let (w1, w2) = (
        got.wavelength.wavelength_at(15.0, 15.0),
        reference.wavelength.wavelength_at(15.0, 15.0),
    );
    assert!((w1 - w2).abs() < 1e-12, "wavelength {w1} vs {w2}");
}
