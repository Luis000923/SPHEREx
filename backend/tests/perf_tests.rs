//! Performance profile and budgets for the processing path (task b9).
//!
//! Measures where the time and memory go between "cutouts arrive" and
//! "JSON leaves" and pins budgets so regressions fail the suite.
//!
//! Run with output:
//!     cargo test --release --test perf_tests -- --nocapture
//!
//! Strict budgets apply to release builds only; in debug the same tests
//! run with relaxed budgets (`perf::DEBUG_FACTOR`) and trimmed sample
//! sizes so the default `cargo test` stays fast. No network: the
//! upstream is the stubbed `FrameSource` with synthetic MEF cutouts.

mod fixtures;
#[path = "support/perf.rs"]
mod perf;
mod support;

use spherex_backend::fits::parse_frame;
use spherex_backend::science::{
    coadd_epochs, image_to_b64, reproject, stack_images, subtract_epochs, Interp, ReprojectOptions,
    StackOptions, DEFAULT_WAVELENGTH_TOLERANCE_UM,
};

use perf::*;

// ---------------------------------------------------------------------
// 1. parse_frame on 29x29, 64x64 and 128x128 synthetic MEF cutouts
//    (IMAGE, VARIANCE, FLAGS, ZODI, WCS-WAVE; tilted TAN+SIP WCS).
//    Median and p95 over 200 runs (release).
// ---------------------------------------------------------------------

#[test]
fn perf_1_parse_frame() {
    let _g = perf_lock();
    let mut rep = Report::new("parse_frame (TAN+SIP MEF cutout)");
    for n in [29usize, 64, 128] {
        let bytes = mef_bytes(&CutoutSpec::realistic(n, 3, 61000.0));
        let st = bench(runs(200), || parse_frame(&bytes).expect("parses"));
        rep.timed(
            "parse_frame",
            &format!("{n}x{n}"),
            &st,
            match n {
                29 => 0.5,
                64 => 1.0,
                _ => 2.0,
            },
        );
    }
    rep.print();
}

// ---------------------------------------------------------------------
// 2. reproject (nearest + bilinear) of one 29x29 TAN+SIP frame onto
//    192x192 and 256x256 output grids. Per-frame time.
// ---------------------------------------------------------------------

#[test]
fn perf_2_reproject() {
    let _g = perf_lock();
    let mut rep = Report::new("reproject one 29x29 frame (TAN+SIP)");
    let frame = perf_frame(&CutoutSpec::realistic(29, 3, 61000.0));
    for npix in [192usize, 256] {
        let grid = out_grid(npix);
        for (name, interp) in [("nearest", Interp::Nearest), ("bilinear", Interp::Bilinear)] {
            let opts = ReprojectOptions {
                interp,
                subtract_zodi: true,
            };
            let st = bench(runs(20), || {
                reproject(&frame, &grid, &opts).expect("reprojects")
            });
            rep.timed(
                &format!("reproject/{name}"),
                &format!("{npix}x{npix}"),
                &st,
                match (interp, npix) {
                    (Interp::Nearest, 192) => 30.0,
                    (Interp::Nearest, _) => 60.0,
                    (_, 192) => 40.0,
                    _ => 80.0,
                },
            );
        }
    }
    rep.print();
}

// ---------------------------------------------------------------------
// 3. stack_images / subtract_epochs / coadd_epochs for 6, 24 and 60
//    frames at 192x192. subtract/coadd are measured as the full pass
//    the API does: (n-1) diffs vs the first frame, and a sequential
//    (n-1)-step pairwise co-add chain.
// ---------------------------------------------------------------------

#[test]
fn perf_3_epoch_ops() {
    let _g = perf_lock();
    let mut rep = Report::new("epoch arithmetic at 192x192");
    let grid = out_grid(192);
    let wave = 1.35;
    let opts = StackOptions::default();
    for n in [6usize, 24, 60] {
        let images: Vec<_> = (0..n)
            .map(|i| sci_image(&grid, i, wave, 61000.0 + i as f64))
            .collect();
        let refs: Vec<_> = images.iter().collect();

        let st = bench(runs(30), || stack_images(&refs, &opts).expect("stacks"));
        rep.timed(
            "stack_images",
            &format!("{n} frames"),
            &st,
            match n {
                6 => 5.0,
                24 => 15.0,
                _ => 40.0,
            },
        );

        let st = bench(runs(30), || {
            for im in &images[1..] {
                std::hint::black_box(
                    subtract_epochs(im, &images[0], DEFAULT_WAVELENGTH_TOLERANCE_UM)
                        .expect("subtracts"),
                );
            }
            images.len()
        });
        rep.timed(
            "subtract_epochs x(n-1)",
            &format!("{n} frames"),
            &st,
            match n {
                6 => 5.0,
                24 => 20.0,
                _ => 45.0,
            },
        );

        let st = bench(runs(30), || {
            let mut acc = coadd_epochs(
                &images[0],
                &images[1],
                DEFAULT_WAVELENGTH_TOLERANCE_UM,
                &opts,
            )
            .expect("coadds");
            for im in &images[2..] {
                acc =
                    coadd_epochs(&acc, im, DEFAULT_WAVELENGTH_TOLERANCE_UM, &opts).expect("coadds");
            }
            std::hint::black_box(acc.dims())
        });
        rep.timed(
            "coadd_epochs x(n-1)",
            &format!("{n} frames"),
            &st,
            match n {
                6 => 8.0,
                24 => 25.0,
                _ => 60.0,
            },
        );
    }
    rep.print();
}

// ---------------------------------------------------------------------
// 4. image_to_b64 + JSON serialisation of the full /frames payload for
//    24 and 60 frames at 192x192 and 256x256; payload size raw and
//    gzip (router CompressionLayer, tower-http default level = 6).
// ---------------------------------------------------------------------

#[tokio::test]
async fn perf_4_encode_payload() {
    let _g = PERF_LOCK.lock().await;
    let mut rep = Report::new("encode + /frames payload");
    for npix in [192usize, 256] {
        // Raster encode cost alone.
        let grid = out_grid(npix);
        let im = sci_image(&grid, 0, 1.35, 61000.0);
        let st = bench(runs(50), || image_to_b64(&im.image));
        rep.timed(
            "image_to_b64",
            &format!("{npix}x{npix}"),
            &st,
            match npix {
                192 => 1.0,
                _ => 2.0,
            },
        );
    }
    for (n, npix) in [(24usize, 192usize), (24, 256), (60, 192), (60, 256)] {
        let (payload, b64_bytes) = frames_payload(n, npix);
        let st = bench(runs(10), || {
            serde_json::to_vec(&payload).expect("serialises")
        });
        let json_len = serde_json::to_vec(&payload).expect("serialises").len();
        rep.timed(
            "serde_json::to_vec",
            &format!("{n} frames @{npix}"),
            &st,
            match (n, npix) {
                (24, 192) => 10.0,
                (24, _) => 12.0,
                (60, 192) => 15.0,
                _ => 40.0,
            },
        );
        rep.note(
            "payload raw JSON",
            &format!("{n} frames @{npix}"),
            &format!("{json_len} B (b64 rasters {b64_bytes} B)"),
        );
    }

    // Raw vs gzip sizes through the real router (production encoding).
    // fov=0.5 makes the configured OUTPUT_NPIX cap (and the 20 MiB
    // payload cap at 60 frames, npix=255) bind, so these stay the true
    // worst-case payload sizes; at fov=0.05 the native-scale rule would
    // silently shrink the raster to 120 px.
    for (n, npix) in [(24usize, 192usize), (60, 256)] {
        let frames = e2e_frames(n);
        let out = e2e_npix(npix);
        let app = perf_router(frames, 29, out);
        let uri = format!(
            "/api/v1/frames?ra={RA_DEG}&dec={DEC_DEG}&fov={FOV_WIDE_DEG}&mode=raw&max_frames={frames}"
        );
        let (status, _, raw, _) = request(&app, &uri, false).await;
        assert_eq!(status, 200);
        let width = body_width(&raw);
        assert_eq!(
            width,
            expected_out_npix(FOV_WIDE_DEG, out, frames),
            "native-scale grid + payload cap picked an unexpected side"
        );
        let (status, hdrs, gz, _) = request(&app, &uri, true).await;
        assert_eq!(status, 200);
        let gzipped = hdrs.get("content-encoding").and_then(|v| v.to_str().ok()) == Some("gzip");
        rep.note(
            "payload via router",
            &format!("{frames} frames @{width} (cap {out})"),
            &format!(
                "raw {} B, gzip {} B ({}x, header {})",
                raw.len(),
                gz.len(),
                raw.len() as f64 / gz.len().max(1) as f64,
                if gzipped { "gzip" } else { "identity" }
            ),
        );
        assert!(gzipped, "router must gzip JSON when accepted");
    }
    rep.print();
}

// ---------------------------------------------------------------------
// 5. End-to-end handler time through the router (zero-latency stub):
//    /frames raw/diff/coadd/stack with 24 frames at the typical request
//    fov=0.05, where the native-scale rule picks 120 px (not the 192 px
//    cap), and /spectrum. CPU-only latency.
// ---------------------------------------------------------------------

#[tokio::test]
async fn perf_5_end_to_end() {
    let _g = PERF_LOCK.lock().await;
    let mut rep = Report::new("end-to-end handler latency (zero-latency upstream)");
    let n = e2e_frames(24);
    let cap = e2e_npix(192);
    let npix = expected_out_npix(FOV_DEG, cap, n);
    let app = perf_router(n, 29, cap);
    for mode in ["raw", "diff", "coadd", "stack"] {
        let uri = format!(
            "/api/v1/frames?ra={RA_DEG}&dec={DEC_DEG}&fov={FOV_DEG}&mode={mode}&max_frames={n}"
        );
        let mut ts = Vec::new();
        for _ in 0..runs(6) {
            let (status, _, body, ms) = request(&app, &uri, false).await;
            assert_eq!(status, 200);
            assert_eq!(body_width(&body), npix);
            std::hint::black_box(body);
            ts.push(ms);
        }
        let st = stats(&ts);
        rep.timed(
            &format!("/frames?mode={mode}"),
            &format!("{n} frames @{npix}"),
            &st,
            match mode {
                "raw" => 350.0,
                _ => 450.0,
            },
        );
    }
    let uri = format!("/api/v1/spectrum?ra={RA_DEG}&dec={DEC_DEG}&aperture_px=3");
    let mut ts = Vec::new();
    for _ in 0..runs(6) {
        let (status, _, body, ms) = request(&app, &uri, false).await;
        assert_eq!(status, 200);
        std::hint::black_box(body);
        ts.push(ms);
    }
    rep.timed("/spectrum", &format!("{n} frames"), &stats(&ts), 100.0);
    rep.print();
}

// ---------------------------------------------------------------------
// 6. Peak memory: VmHWM delta across a 60-frame request at the widest
//    FOV, where the 256 cap and the 20 MiB payload cap bind (255 px).
// ---------------------------------------------------------------------

#[tokio::test]
async fn perf_6_peak_memory() {
    let _g = PERF_LOCK.lock().await;
    let mut rep = Report::new("peak memory (VmHWM)");
    let n = e2e_frames(60);
    let cap = e2e_npix(256);
    let npix = expected_out_npix(FOV_WIDE_DEG, cap, n);
    let app = perf_router(n, 29, cap);
    let uri = format!(
        "/api/v1/frames?ra={RA_DEG}&dec={DEC_DEG}&fov={FOV_WIDE_DEG}&mode=raw&max_frames={n}"
    );
    // No warm-up: the delta over the first requests captures the
    // workload's true peak on top of the process baseline.
    let before = vm_hwm_bytes();
    let mut payload_bytes = 0usize;
    for _ in 0..3 {
        let (status, _, body, _) = request(&app, &uri, false).await;
        assert_eq!(status, 200);
        assert_eq!(body_width(&body), npix);
        payload_bytes = body.len();
        drop(body);
    }
    let after = vm_hwm_bytes();
    let delta = after.saturating_sub(before);
    let budget = budget_mib(256) * 1024 * 1024;
    rep.note(
        "VmHWM delta, 3 requests",
        &format!("{n} frames @{npix}, payload {payload_bytes} B"),
        &format!(
            "{} MiB (peak {} MiB)",
            delta / (1024 * 1024),
            after / (1024 * 1024)
        ),
    );
    rep.print();
    assert!(
        delta <= budget,
        "VmHWM grew {} MiB over a {n}-frame @{npix} request (budget {} MiB)",
        delta / (1024 * 1024),
        budget / (1024 * 1024)
    );
}

// ---------------------------------------------------------------------
// 7. Concurrency: 8 simultaneous /frames requests vs sequential wall
//    time, plus a peak-memory bound.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn perf_7_concurrency() {
    let _g = PERF_LOCK.lock().await;
    let mut rep = Report::new("concurrency: 8x /frames");
    let n = e2e_frames(24);
    let cap = e2e_npix(192);
    // fov=0.5: each request really works on the 192 px worst case
    // (payload cap is 404 px at 24 frames), so the memory bound is
    // measured against full-size rasters, not the 120 px typical case.
    let npix = expected_out_npix(FOV_WIDE_DEG, cap, n);
    let app = perf_router(n, 29, cap);
    let uri = format!(
        "/api/v1/frames?ra={RA_DEG}&dec={DEC_DEG}&fov={FOV_WIDE_DEG}&mode=raw&max_frames={n}"
    );

    // Sequential baseline: sum of 8 one-at-a-time requests.
    let mut seq_ms = 0.0;
    for _ in 0..8 {
        let (status, _, body, ms) = request(&app, &uri, false).await;
        assert_eq!(status, 200);
        assert_eq!(body_width(&body), npix);
        std::hint::black_box(body);
        seq_ms += ms;
    }

    let before = vm_hwm_bytes();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let app = app.clone();
        let uri = uri.clone();
        set.spawn(async move { request(&app, &uri, false).await });
    }
    let t = std::time::Instant::now();
    let mut ok = 0usize;
    let mut payload = 0usize;
    while let Some(r) = set.join_next().await {
        let (status, _, body, _) = r.expect("request task joins");
        assert_eq!(status, 200);
        ok += 1;
        payload += body.len();
    }
    let conc_ms = t.elapsed().as_secs_f64() * 1e3;
    let delta = vm_hwm_bytes().saturating_sub(before);
    let mem_budget = budget_mib(384) * 1024 * 1024;

    rep.note(
        "8x sequential sum",
        &format!("{n} frames @{npix}"),
        &format!("{seq_ms:.0} ms"),
    );
    rep.note(
        "8x concurrent wall",
        &format!("{n} frames @{npix}"),
        &format!(
            "{conc_ms:.0} ms ({:.1}x, {ok} ok, {payload} B, VmHWM +{} MiB)",
            seq_ms / conc_ms.max(1.0),
            delta / (1024 * 1024)
        ),
    );
    rep.print();

    assert_eq!(ok, 8);
    assert!(
        delta <= mem_budget,
        "VmHWM grew {} MiB across 8 concurrent requests (budget {} MiB)",
        delta / (1024 * 1024),
        mem_budget / (1024 * 1024)
    );
    if cfg!(not(debug_assertions)) {
        assert!(
            conc_ms < seq_ms,
            "concurrent wall {conc_ms:.0} ms not below sequential {seq_ms:.0} ms"
        );
    }
}

// ---------------------------------------------------------------------
// Stats over externally-collected wall times come from perf::stats.
// ---------------------------------------------------------------------
