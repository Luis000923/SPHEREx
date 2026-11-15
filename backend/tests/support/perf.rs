//! Shared plumbing for the b9 performance-profile tests
//! (`perf_tests.rs` loads this file via `#[path]`).
//!
//! Everything runs against the stubbed `FrameSource` and synthetic MEF
//! cutouts from `tests/fixtures`; no test reaches the real IRSA.
//!
//! Budget convention: strict numbers gate release runs
//! (`cargo test --release --test perf_tests`); debug runs apply
//! [`DEBUG_FACTOR`] so the default `cargo test` stays meaningful and
//! fast. Timings are machine-dependent; budgets carry ~2x headroom over
//! the values measured on the dev box (see the b9 handoff for the raw
//! table).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde::Serialize;
use spherex_backend::api::{build_router, AppState};
use spherex_backend::fits::{parse_frame, Frame};
use spherex_backend::science::{image_to_b64, OutputGrid, ScienceImage};
use tower::ServiceExt;

use crate::fixtures::*;
use crate::support::{obs_id_hash, stub_observations, StubSource, TEST_ADDR};

// ---------------------------------------------------------------------
// Scenario constants (the b3 live-verified field).
// ---------------------------------------------------------------------

pub const RA_DEG: f64 = 337.878;
pub const DEC_DEG: f64 = -5.189;
pub const FOV_DEG: f64 = 0.05;
/// Widest legal request FOV (contract max). At this width the
/// native-scale rule (`api::frames::output_npix`, OVERSAMPLE x4)
/// wants ~1160 px for the stub cutouts, so the configured
/// `OUTPUT_NPIX` cap — and the 20 MiB payload cap — are what bind.
/// Used wherever a test needs the true worst-case raster side.
pub const FOV_WIDE_DEG: f64 = 0.5;

/// Representative central wavelength per detector array (index 1..=6).
/// Real SPHEREx bands do not overlap, so each array gets a distinct
/// value inside its range.
pub const DET_WAVE_UM: [f64; 7] = [0.0, 0.95, 1.35, 2.00, 3.10, 4.00, 4.70];

/// Debug builds run the same pipeline far slower; strict budgets are
/// only meaningful in release. Measured debug/release ratio on the dev
/// box is ~10-30x depending on the operation, so 40x keeps the debug
/// pass as a smoke check rather than a timing gate.
pub const DEBUG_FACTOR: f64 = 40.0;

/// Iteration counts per profile: release gets the statistical sample
/// the task asks for; debug trims the slow paths so `cargo test` stays
/// fast (correctness is identical, only the sample size changes).
pub fn runs(release_runs: usize) -> usize {
    if cfg!(not(debug_assertions)) {
        release_runs
    } else {
        (release_runs / 10).max(2)
    }
}

/// Strict budget for release; relaxed in debug.
pub fn budget_ms(strict_ms: f64) -> f64 {
    if cfg!(not(debug_assertions)) {
        strict_ms
    } else {
        strict_ms * DEBUG_FACTOR
    }
}

/// Memory budget in MiB. Debug builds do not inflate allocations the
/// way they inflate runtimes, so the relaxation is much smaller.
pub fn budget_mib(strict_mib: u64) -> u64 {
    if cfg!(not(debug_assertions)) {
        strict_mib
    } else {
        strict_mib * 4
    }
}

/// Workload scaling for the heavy end-to-end tests: in debug a single
/// 24x192 /frames call can take tens of seconds, so the debug pass uses
/// a smaller workload purely as a correctness/budget smoke run.
pub fn e2e_frames(release_n: usize) -> usize {
    if cfg!(not(debug_assertions)) {
        release_n
    } else {
        6
    }
}

/// Output raster side for e2e/memory/concurrency tests.
pub fn e2e_npix(release_n: usize) -> usize {
    if cfg!(not(debug_assertions)) {
        release_n
    } else {
        96
    }
}

// ---------------------------------------------------------------------
// Serialisation guard: timing tests run one at a time inside the
// default multi-threaded test harness so they do not skew each other.
// ---------------------------------------------------------------------

/// Global serialisation point for the perf tests. Sync tests take
/// [`perf_lock`]; async tests `PERF_LOCK.lock().await` (a tokio guard
/// is `Send`, so it can be held across awaits on the multi-thread
/// flavour).
pub static PERF_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Blocking acquire for non-async tests (never called inside a runtime).
pub fn perf_lock() -> tokio::sync::MutexGuard<'static, ()> {
    PERF_LOCK.blocking_lock()
}

// ---------------------------------------------------------------------
// Timing statistics.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub runs: usize,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
}

/// Time `runs` executions of `f` (return values are black-boxed away).
pub fn bench<F, R>(n: usize, mut f: F) -> Stats
where
    F: FnMut() -> R,
{
    let mut ts = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        let _ = std::hint::black_box(f());
        ts.push(t.elapsed().as_secs_f64() * 1e3);
    }
    stats(&ts)
}

/// One wall-clock measurement.
pub fn once<F, R>(f: F) -> (R, f64)
where
    F: FnOnce() -> R,
{
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64() * 1e3)
}

/// Median/p95/min/max/mean over externally-collected millisecond
/// samples (async paths that cannot go through [`bench`]).
pub fn stats(samples: &[f64]) -> Stats {
    debug_assert!(!samples.is_empty());
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    let pct = |p: f64| -> f64 {
        let i = ((n - 1) as f64 * p).round() as usize;
        s[i.min(n - 1)]
    };
    Stats {
        runs: n,
        median_ms: pct(0.5),
        p95_ms: pct(0.95),
        min_ms: s[0],
        max_ms: s[n - 1],
        mean_ms: s.iter().sum::<f64>() / n as f64,
    }
}

// ---------------------------------------------------------------------
// Report table (printed with --nocapture; the handoff reuses it).
// ---------------------------------------------------------------------

pub struct Report {
    title: String,
    rows: Vec<String>,
}

impl Report {
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_string(),
            rows: Vec::new(),
        }
    }

    /// Record one timed row and enforce its p95 budget (release-strict,
    /// relaxed in debug).
    pub fn timed(&mut self, op: &str, size: &str, st: &Stats, strict_budget_ms: f64) {
        let budget = budget_ms(strict_budget_ms);
        let ok = st.p95_ms <= budget;
        self.rows.push(format!(
            "| {op} | {size} | {} | {:.2} | {:.2} | {:.0} | {} |",
            st.runs,
            st.median_ms,
            st.p95_ms,
            budget,
            if ok { "ok" } else { "OVER" }
        ));
        assert!(
            ok,
            "perf budget exceeded: {op} ({size}) p95 {:.2} ms > {:.0} ms budget",
            st.p95_ms, budget
        );
    }

    /// Record a size/count observation (no budget).
    pub fn note(&mut self, op: &str, size: &str, detail: &str) {
        self.rows
            .push(format!("| {op} | {size} | - | {detail} | - | - | - |"));
    }

    pub fn print(&self) {
        println!("\n=== {} ===", self.title);
        println!("| operation | size | runs | median ms | p95 ms | budget ms | status |");
        println!("|---|---|---|---|---|---|---|");
        for r in &self.rows {
            println!("{r}");
        }
    }
}

// ---------------------------------------------------------------------
// Synthetic MEF cutout builder (IMAGE/FLAGS/VARIANCE/ZODI/WCS-WAVE,
// tilted TAN + SIP WCS). Deterministic content, no network.
// ---------------------------------------------------------------------

pub struct CutoutSpec {
    /// Square cutout side (pixels).
    pub npix: usize,
    /// DETECTOR keyword (1..=6).
    pub detector: i64,
    /// Wavelength at the left edge control point (um); slopes gently
    /// across x like the real spectral gradient.
    pub wave_um: f64,
    /// MJD-AVG / MJD-OBS.
    pub mjd: f64,
    /// CRVAL (cutout centre).
    pub ra_deg: f64,
    pub dec_deg: f64,
    /// CD rotation (degrees of tilt).
    pub tilt_deg: f64,
    /// Pixel scale (deg/px).
    pub scale_deg: f64,
    /// Background flux level (MJy/sr).
    pub flux: f32,
    /// Constant variance level.
    pub variance: f32,
    /// ZODI model level.
    pub zodi: f32,
    /// Emit SIP (A_/B_) + inverse (AP_/BP_) distortion keywords.
    pub sip: bool,
}

impl CutoutSpec {
    /// ~29 px cutout (~0.05 deg at the real ~6.15"/px scale).
    pub fn realistic(npix: usize, detector: i64, mjd: f64) -> Self {
        Self {
            npix,
            detector,
            wave_um: DET_WAVE_UM[detector.clamp(1, 6) as usize],
            mjd,
            ra_deg: RA_DEG,
            dec_deg: DEC_DEG,
            tilt_deg: 15.0,
            scale_deg: 0.05 / 29.0,
            flux: 12.5,
            variance: 0.5,
            zodi: 8.0,
            sip: true,
        }
    }
}

/// Deterministic pseudo-texture (no rand dependency): smooth field plus
/// a faint grating and a point source so codecs do real work.
fn tex(x: usize, y: usize, seed: usize) -> f32 {
    let h = (x
        .wrapping_mul(31)
        .wrapping_add(y.wrapping_mul(17))
        .wrapping_add(seed))
        % 97;
    let mut v = h as f32 * 0.002;
    if x == y {
        v += 40.0;
    }
    v
}

/// Build a synthetic MEF cutout (`parse_frame`-ready bytes).
pub fn mef_bytes(spec: &CutoutSpec) -> Vec<u8> {
    let n = spec.npix;
    let mut image = Vec::with_capacity(n * n);
    let mut variance = Vec::with_capacity(n * n);
    let mut zodi = Vec::with_capacity(n * n);
    let mut flags = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            image.push(spec.flux + tex(x, y, n));
            variance.push(spec.variance + 0.001 * tex(x, y, 7));
            zodi.push(spec.zodi - 0.02 * x as f32);
            flags.push(if (x + 3 * y).is_multiple_of(211) {
                1
            } else {
                0
            });
        }
    }

    let t = spec.tilt_deg.to_radians();
    let (c, s) = (t.cos(), t.sin());
    let p = spec.scale_deg;
    let mut wcs = vec![
        card_str("CTYPE1", if spec.sip { "RA---TAN-SIP" } else { "RA---TAN" }),
        card_str("CTYPE2", if spec.sip { "DEC--TAN-SIP" } else { "DEC--TAN" }),
        card_flt("CRPIX1", n as f64 / 2.0 + 0.5),
        card_flt("CRPIX2", n as f64 / 2.0 + 0.5),
        card_flt("CRVAL1", spec.ra_deg),
        card_flt("CRVAL2", spec.dec_deg),
        card_flt("CD1_1", -p * c),
        card_flt("CD1_2", p * s),
        card_flt("CD2_1", p * s),
        card_flt("CD2_2", p * c),
        card_int("DETECTOR", spec.detector),
        card_flt("MJD-AVG", spec.mjd),
        card_flt("MJD-OBS", spec.mjd - 0.0014),
        card_str("BUNIT", "MJy / sr"),
    ];
    if spec.sip {
        // Second-order SIP + inverse, magnitudes like real SPHEREx SIP.
        wcs.extend([
            card_int("A_ORDER", 2),
            card_int("B_ORDER", 2),
            card_flt("A_2_0", 1.2e-6),
            card_flt("A_1_1", -6.0e-7),
            card_flt("A_0_2", 9.0e-7),
            card_flt("B_2_0", 8.0e-7),
            card_flt("B_1_1", 5.0e-7),
            card_flt("B_0_2", -1.1e-6),
            card_int("AP_ORDER", 2),
            card_int("BP_ORDER", 2),
            card_flt("AP_2_0", -1.2e-6),
            card_flt("AP_1_1", 6.0e-7),
            card_flt("AP_0_2", -9.0e-7),
            card_flt("BP_2_0", -8.0e-7),
            card_flt("BP_1_1", -5.0e-7),
            card_flt("BP_0_2", 1.1e-6),
        ]);
    }

    let legend = vec![
        card_str("MSKN0000", "TRANSIENT"),
        card_str("MSKD0000", "Transient source in the pixel"),
        card_str("MSKN0001", "SATURATED"),
        card_str("MSKD0001", "Pixel saturated"),
        card_str("MSKN0004", "HOT"),
        card_str("MSKD0004", "Hot pixel"),
    ];

    let mut f = primary_hdu();
    f.extend(image_hdu("IMAGE", -32, n, n, &f32_data(&image), &wcs));
    f.extend(image_hdu("FLAGS", 32, n, n, &i32_data(&flags), &legend));
    f.extend(image_hdu("VARIANCE", -32, n, n, &f32_data(&variance), &[]));
    f.extend(image_hdu("ZODI", -32, n, n, &f32_data(&zodi), &[]));

    // WCS-WAVE: 3x3 control grid, (wavelength, bandwidth) pairs,
    // wavelength drifting across x (x fastest in VALUES).
    let xs = [1i32, (n / 2 + 1) as i32, n as i32];
    let ys = xs;
    let mut values = Vec::with_capacity(2 * xs.len() * ys.len());
    for _y in &ys {
        for (i, _x) in xs.iter().enumerate() {
            let w = spec.wave_um + 0.002 * i as f64;
            values.push(w as f32);
            values.push((w / 40.0) as f32);
        }
    }
    f.extend(wcs_wave_hdu(&xs, &ys, &values, &[]));
    f
}

/// Parse a synthetic cutout (panics on builder bugs, as fixtures do).
pub fn perf_frame(spec: &CutoutSpec) -> Frame {
    parse_frame(&mef_bytes(spec)).expect("synthetic perf cutout must parse")
}

// ---------------------------------------------------------------------
// Synthetic ScienceImage on the shared output grid (skips the FITS
// decode; used for the epoch-arithmetic benches).
// ---------------------------------------------------------------------

pub fn sci_image(grid: &OutputGrid, seed: usize, wave_um: f64, mjd: f64) -> ScienceImage {
    let mut im = ScienceImage::blank(*grid);
    for y in 0..grid.ny {
        for x in 0..grid.nx {
            im.image[[y, x]] = 10.0 + tex(x, y, seed);
            im.variance[[y, x]] = 0.4 + 0.001 * tex(x, y, seed + 3);
            im.flags[[y, x]] = if (x + y + seed).is_multiple_of(211) {
                1
            } else {
                0
            };
            im.n_contrib[[y, x]] = 1;
        }
    }
    im.detector = Some(3);
    im.wavelength_um = wave_um;
    im.mjd = Some(mjd);
    im.zodi_subtracted = true;
    im
}

/// The grid the API uses: square `npix`, centred on the query.
pub fn out_grid(npix: usize) -> OutputGrid {
    OutputGrid::centered(RA_DEG, DEC_DEG, FOV_DEG, npix).expect("valid grid")
}

// ---------------------------------------------------------------------
// Stub source + router at production-like sizes.
// ---------------------------------------------------------------------

/// `n` observations (detectors cycling D1..D6, wavelengths matching the
/// array) whose cutouts are `cutout_npix` on a side.
pub fn perf_source(n: usize, cutout_npix: usize) -> StubSource {
    let observations = stub_observations(n);
    let mut frames = HashMap::new();
    for (i, o) in observations.iter().enumerate() {
        let det = (i % 6 + 1) as i64;
        let spec = CutoutSpec::realistic(cutout_npix, det, 61000.0 + i as f64);
        frames.insert(obs_id_hash(o), Arc::new(perf_frame(&spec)) as Arc<Frame>);
    }
    StubSource {
        observations,
        frames,
        fetch_fail: None,
        search_error: None,
        delays: HashMap::new(),
        in_flight: Arc::new(AtomicUsize::new(0)),
        fetch_calls: Arc::new(AtomicUsize::new(0)),
    }
}

pub fn perf_state(source: StubSource, output_npix: usize) -> AppState<StubSource> {
    AppState {
        source: Arc::new(source),
        collections: Arc::new(vec!["spherex_qr3".to_string()]),
        static_dir: PathBuf::from("frontend/dist"),
        allowed_origins: Arc::new(Vec::new()),
        output_npix,
        max_frames_cap: 60,
        // CPU-only timing: the deadline must never fire.
        request_deadline: Duration::from_secs(600),
        soft_deadline: Duration::from_secs(600),
        rate_limit_rps: 1_000_000,
    }
}

pub fn perf_router(n_frames: usize, cutout_npix: usize, out_npix: usize) -> Router {
    build_router(perf_state(perf_source(n_frames, cutout_npix), out_npix))
}

// ---------------------------------------------------------------------
// Request helpers.
// ---------------------------------------------------------------------

/// One GET through the router; returns status, headers, body, wall ms.
/// `gzip = true` sends `Accept-Encoding: gzip` so the response body is
/// the production compressed payload (tower-http, default level 6).
pub async fn request(
    app: &Router,
    uri: &str,
    gzip: bool,
) -> (StatusCode, axum::http::HeaderMap, Bytes, f64) {
    let mut b = Request::builder().method(Method::GET).uri(uri);
    if gzip {
        b = b.header("accept-encoding", "gzip");
    }
    let mut req = b.body(Body::empty()).expect("request builds");
    req.extensions_mut().insert(ConnectInfo(TEST_ADDR));
    let t = Instant::now();
    let res = app.clone().oneshot(req).await.expect("router answers");
    let status = res.status();
    let headers = res.headers().clone();
    let body = to_bytes(res.into_body(), 64 * 1024 * 1024)
        .await
        .expect("body reads");
    (status, headers, body, t.elapsed().as_secs_f64() * 1e3)
}

/// `width` field of a `/frames` response body: the raster side the
/// native-scale rule actually picked (120 px at fov=0.05 with the
/// ~6.2"/px stub cutouts, the configured cap at fov=0.5).
pub fn body_width(body: &[u8]) -> usize {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["width"].as_u64())
        .unwrap_or(0) as usize
}

/// Raster side the handler must choose for `fov_deg`, a configured
/// `OUTPUT_NPIX` of `cap` and `n_frames` decoded cutouts. Mirrors
/// `api::frames::output_npix` + `payload_npix_cap` (both pub(crate)):
/// `ceil(fov/scale*OVERSAMPLE/8)*8` clamped to `[32, cap]`, then
/// limited so `n * (npix^2 * 16/3 + 2048) < 20 MiB`. The stub cutout
/// scale is `CutoutSpec::realistic`'s `0.05/29` deg/px.
pub fn expected_out_npix(fov_deg: f64, cap: usize, n_frames: usize) -> usize {
    let scale = 0.05 / 29.0;
    let wanted = ((fov_deg / scale * 4.0) / 8.0).ceil() * 8.0;
    let native = (wanted as usize).clamp(32usize.min(cap), cap);
    let budget = (20 * 1024 * 1024usize).saturating_sub(n_frames * 2048);
    let payload_cap = ((budget as f64 / n_frames.max(1) as f64) * 3.0 / 16.0)
        .sqrt()
        .max(1.0) as usize;
    native.min(payload_cap)
}

// ---------------------------------------------------------------------
// Peak RSS (Linux /proc/self/status VmHWM).
// ---------------------------------------------------------------------

/// High-water mark of resident memory in bytes (0 when unreadable).
/// VmHWM is monotonic within a process, so before/after deltas capture
/// the *new* peak a workload added.
pub fn vm_hwm_bytes() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------
// /frames payload mirror (DTO types are pub(crate); this mirrors the
// contract schema field-for-field so serialisation cost is faithful).
// ---------------------------------------------------------------------

#[derive(Serialize)]
pub struct PayloadWcs {
    pub crpix: [f64; 2],
    pub crval: [f64; 2],
    pub cd: [[f64; 2]; 2],
}

#[derive(Serialize)]
pub struct PayloadFrame {
    pub id: String,
    pub obs_mjd_start: f64,
    pub obs_mjd_end: f64,
    pub detector: u8,
    pub wavelength_um: f64,
    pub bandwidth_um: f64,
    pub collection: String,
    pub zodi_subtracted: bool,
    pub flags: Vec<String>,
    pub wcs: PayloadWcs,
    pub data_b64: String,
}

#[derive(Serialize)]
pub struct PayloadQuery {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub fov_deg: f64,
    pub mode: String,
}

#[derive(Serialize)]
pub struct PayloadResponse {
    pub query: PayloadQuery,
    pub width: usize,
    pub height: usize,
    pub unit: String,
    pub frames: Vec<PayloadFrame>,
    pub flag_legend: std::collections::BTreeMap<String, String>,
    pub truncated: bool,
    pub total_observations: usize,
}

/// Build a contract-shaped `/frames` payload: `n` synthetic rasters on
/// an `npix` grid, each base64-encoded. Returns the response plus the
/// total base64 raster bytes (for reporting).
pub fn frames_payload(n: usize, npix: usize) -> (PayloadResponse, usize) {
    let grid = out_grid(npix);
    let mut frames = Vec::with_capacity(n);
    let mut b64_bytes = 0usize;
    for i in 0..n {
        let det = (i % 6 + 1) as u8;
        let im = sci_image(&grid, i, DET_WAVE_UM[det as usize], 61000.0 + i as f64);
        let data_b64 = image_to_b64(&im.image);
        b64_bytes += data_b64.len();
        frames.push(PayloadFrame {
            id: format!("{i:016x}"),
            obs_mjd_start: 61000.0 + i as f64,
            obs_mjd_end: 61000.0 + i as f64 + 0.0014,
            detector: det,
            wavelength_um: im.wavelength_um,
            bandwidth_um: 0.05,
            collection: "spherex_qr3".to_string(),
            zodi_subtracted: false,
            flags: vec!["TRANSIENT".to_string()],
            wcs: PayloadWcs {
                crpix: grid.wcs.crpix,
                crval: grid.wcs.crval,
                cd: grid.wcs.cd,
            },
            data_b64,
        });
    }
    let mut flag_legend = std::collections::BTreeMap::new();
    flag_legend.insert("0".to_string(), "Transient source in the pixel".to_string());
    flag_legend.insert("1".to_string(), "Pixel saturated".to_string());
    (
        PayloadResponse {
            query: PayloadQuery {
                ra_deg: RA_DEG,
                dec_deg: DEC_DEG,
                fov_deg: FOV_DEG,
                mode: "raw".to_string(),
            },
            width: npix,
            height: npix,
            unit: "MJy/sr".to_string(),
            frames,
            flag_legend,
            truncated: false,
            total_observations: n,
        },
        b64_bytes,
    )
}
