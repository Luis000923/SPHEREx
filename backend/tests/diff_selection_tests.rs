//! Tests for the mode-aware epoch selection (task b13).
//!
//! `diff`/`coadd`/`stack` only merge frames of the same detector AND
//! wavelength within 0.05 um (`science::group_frames`), so
//! [`Observation::select_comparable`] concentrates the frame budget on
//! the largest detector x wavelength buckets instead of spreading it
//! evenly over the survey like `select_frames` (which `raw` keeps — it
//! is the time-lapse). All fixtures are synthetic; the single live
//! check is opt-in via `SPHEREX_LIVE=1`.

mod fixtures;
mod support;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use serde_json::Value;
use spherex_backend::fits::parse_frame;
use spherex_backend::irsa::{select_frames, IrsaClient, Observation};
use support::*;

/// Wavelength clustering tolerance used by `group_frames` (b4).
const TOL_UM: f64 = 0.05;

/// One synthetic SIA row: detector `det` via `energy_bandpassname`,
/// band centre `wave_um` (`em_min`/`em_max` = centre +/- 0.0125 um),
/// exposure at MJD `t_mjd`.
fn obs(det: Option<u8>, wave_um: f64, t_mjd: f64, url: String) -> Observation {
    Observation {
        t_min_mjd: t_mjd,
        t_max_mjd: t_mjd + 0.0014,
        em_min_um: wave_um - 0.0125,
        em_max_um: wave_um + 0.0125,
        band_name: det.map(|d| format!("SPHEREx-D{d}")).unwrap_or_default(),
        access_url: url,
        collection: "spherex_qr3".to_string(),
    }
}

/// Mid-exposure epoch (mirrors the private `Observation::epoch`).
fn epoch(o: &Observation) -> f64 {
    match (o.t_min_mjd.is_finite(), o.t_max_mjd.is_finite()) {
        (true, true) => f64::midpoint(o.t_min_mjd, o.t_max_mjd),
        (true, false) => o.t_min_mjd,
        _ => o.t_max_mjd,
    }
}

/// Largest comparable group inside a selection `(n_epochs, baseline_d)`
/// using the documented bucketing: detector hint, then wavelength
/// bins of `TOL_UM` anchored on the first member (same rule as
/// `group_frames`). Used to score a selection, not to re-test the
/// bucketing itself.
fn largest_bucket(sel: &[Observation]) -> (usize, f64) {
    let mut by_det: BTreeMap<Option<u8>, Vec<&Observation>> = BTreeMap::new();
    for o in sel {
        by_det.entry(o.detector_hint()).or_default().push(o);
    }
    let (mut best_n, mut best_base) = (0usize, 0.0f64);
    let mut record = |members: &[&Observation]| {
        if members.len() > best_n {
            best_n = members.len();
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for o in members {
                let e = epoch(o);
                if e.is_finite() {
                    lo = lo.min(e);
                    hi = hi.max(e);
                }
            }
            best_base = if hi >= lo { hi - lo } else { 0.0 };
        }
    };
    for (_d, mut rows) in by_det {
        rows.sort_by(|a, b| a.band_center_um().total_cmp(&b.band_center_um()));
        let mut members: Vec<&Observation> = Vec::new();
        let mut anchor = f64::NAN;
        for o in rows {
            let w = o.band_center_um();
            if members.is_empty() {
                anchor = w;
                members.push(o);
            } else if (w - anchor).abs() <= TOL_UM {
                members.push(o);
            } else {
                record(&members);
                anchor = w;
                members = vec![o];
            }
        }
        record(&members);
    }
    (best_n, best_base)
}

/// Access URLs of a selection, for equality checks.
fn urls(sel: &[Observation]) -> Vec<String> {
    sel.iter().map(|o| o.access_url.clone()).collect()
}

// ---------------------------------------------------------------------
// Pre-download metadata parsing
// ---------------------------------------------------------------------

#[test]
fn detector_hint_parses_band_name() {
    let o = |band: &str| Observation {
        band_name: band.to_string(),
        ..obs(Some(3), 2.0, 61000.0, "u".to_string())
    };
    assert_eq!(o("SPHEREx-D3").detector_hint(), Some(3));
    assert_eq!(o("SPHEREx-D1").detector_hint(), Some(1));
    assert_eq!(o("spherex-d6").detector_hint(), Some(6));
    assert_eq!(o("").detector_hint(), None);
    assert_eq!(o("SPHEREx").detector_hint(), None);
    assert_eq!(o("SPHEREx-D10").detector_hint(), None);
    assert_eq!(o("SPHEREx-D3-extra").detector_hint(), None);
    assert_eq!(o("OTHER-BAND").detector_hint(), None);
}

#[test]
fn band_center_is_midpoint_of_em_bounds() {
    let o = obs(Some(1), 2.0, 61000.0, "u".to_string());
    assert_eq!(o.band_center_um(), 2.0);
    let nan = Observation {
        em_min_um: f64::NAN,
        em_max_um: f64::NAN,
        ..obs(Some(1), 2.0, 61000.0, "u".to_string())
    };
    assert!(nan.band_center_um().is_nan());
}

// ---------------------------------------------------------------------
// Grouped selection maximises comparable frames
// ---------------------------------------------------------------------

/// Six detectors x four wavelength bins, one dominant bucket
/// (D3 @ 2.0 um, 30 epochs over 400 d) and two epochs everywhere else.
fn dominant_bucket_scene() -> Vec<Observation> {
    let mut all = Vec::new();
    for i in 0..30 {
        all.push(obs(
            Some(3),
            2.0,
            61000.0 + i as f64 * (400.0 / 29.0),
            format!("https://stub.invalid/d3/{i}.fits"),
        ));
    }
    let mut k = 0usize;
    for det in 1..=6u8 {
        for wave in [1.0, 2.0, 3.0, 4.0] {
            if det == 3 && wave == 2.0 {
                continue;
            }
            for _ in 0..2 {
                all.push(obs(
                    Some(det),
                    wave,
                    61000.0 + (k * 137 % 400) as f64,
                    format!("https://stub.invalid/o/{k}.fits"),
                ));
                k += 1;
            }
        }
    }
    all
}

#[test]
fn comparable_selection_maximises_group_size_and_baseline() {
    let all = dominant_bucket_scene();
    assert_eq!(all.len(), 76);
    let sel = Observation::select_comparable(&all, 24);
    assert_eq!(sel.len(), 24);
    // Every picked epoch belongs to the winning bucket.
    for o in &sel {
        assert_eq!(o.detector_hint(), Some(3));
        assert!((o.band_center_um() - 2.0).abs() <= TOL_UM);
    }
    // Task criteria: largest group >= min(24, available = 30) and
    // baseline >= 80% of the bucket's 400 d baseline (the even spread
    // includes both endpoints, so the full 400 d is kept).
    let (n, base) = largest_bucket(&sel);
    assert_eq!(n, 24);
    assert!(base >= 0.8 * 400.0, "baseline {base} d");
    // Newest first, same contract as `select_frames`.
    for w in sel.windows(2) {
        assert!(epoch(&w[0]) >= epoch(&w[1]));
    }
    // Before/after on this scene: the old even spread dilutes the
    // dominant bucket to a fraction of the budget.
    let (spread_n, _) = largest_bucket(&select_frames(&all, 24));
    assert!(
        spread_n < n,
        "test scene must exercise the defect (spread gives {spread_n})"
    );
}

#[test]
fn comparable_selection_fills_budget_across_top_buckets() {
    // Six detectors x four wavelengths x eight epochs: every bucket
    // qualifies (>= 3); D2 @ 3.0 um gets the longest baseline and must
    // be ranked first.
    let mut all = Vec::new();
    let mut k = 0usize;
    for det in 1..=6u8 {
        for wave in [1.0, 2.0, 3.0, 4.0] {
            let span = if det == 2 && wave == 3.0 {
                500.0
            } else {
                100.0
            };
            for i in 0..8 {
                all.push(obs(
                    Some(det),
                    wave,
                    61000.0 + i as f64 * (span / 7.0),
                    format!("https://stub.invalid/o/{k}.fits"),
                ));
                k += 1;
            }
        }
    }
    assert_eq!(all.len(), 192);
    let sel = Observation::select_comparable(&all, 24);
    assert_eq!(sel.len(), 24);
    // min(24, 8) from the best bucket, then the next two buckets.
    let best: Vec<&Observation> = sel
        .iter()
        .filter(|o| o.detector_hint() == Some(2) && (o.band_center_um() - 3.0).abs() <= TOL_UM)
        .collect();
    assert_eq!(best.len(), 8);
    let (n, _) = largest_bucket(&sel);
    assert_eq!(n, 8, "largest group must be min(24, available = 8)");
}

#[test]
fn raw_selection_still_spreads_over_the_survey() {
    // Regression: `select_frames` (used by `raw`) is unchanged — an
    // even spread across detectors, not a single bucket.
    let all = dominant_bucket_scene();
    let sel = select_frames(&all, 24);
    assert_eq!(sel.len(), 24);
    let dets: std::collections::HashSet<Option<u8>> =
        sel.iter().map(|o| o.detector_hint()).collect();
    assert!(dets.len() >= 3, "raw must stay spread, got {dets:?}");
    // It also keeps its newest-first ordering.
    for w in sel.windows(2) {
        assert!(epoch(&w[0]) >= epoch(&w[1]));
    }
}

// ---------------------------------------------------------------------
// Fallback and degenerate inputs
// ---------------------------------------------------------------------

#[test]
fn comparable_selection_falls_back_without_a_qualified_bucket() {
    // Every observation in its own detector x wavelength bucket.
    let singles: Vec<Observation> = (0..20)
        .map(|i| {
            obs(
                Some((i % 6 + 1) as u8),
                1.0 + i as f64 * 0.5,
                61000.0 + i as f64,
                format!("https://stub.invalid/s/{i}.fits"),
            )
        })
        .collect();
    assert_eq!(
        urls(&Observation::select_comparable(&singles, 7)),
        urls(&select_frames(&singles, 7)),
        "no bucket >= 3 epochs -> even spread"
    );
    // Buckets of two are also below the threshold.
    let pairs: Vec<Observation> = (0..8)
        .map(|i| {
            obs(
                Some(((i / 2) % 6 + 1) as u8),
                1.0 + (i / 2) as f64 * 0.5,
                61000.0 + i as f64,
                format!("https://stub.invalid/p/{i}.fits"),
            )
        })
        .collect();
    assert_eq!(
        urls(&Observation::select_comparable(&pairs, 4)),
        urls(&select_frames(&pairs, 4))
    );
}

#[test]
fn comparable_selection_degenerate_inputs_never_panic() {
    let empty: Vec<Observation> = Vec::new();
    assert!(Observation::select_comparable(&empty, 24).is_empty());

    let one = vec![obs(Some(2), 1.5, 61000.0, "u".to_string())];
    assert_eq!(Observation::select_comparable(&one, 24).len(), 1);

    // `max = 0` mirrors `select_frames` (returns the sorted set).
    let all = dominant_bucket_scene();
    assert_eq!(
        urls(&Observation::select_comparable(&all, 0)),
        urls(&select_frames(&all, 0))
    );
    // Everything fits: the whole set is kept, no concentration needed.
    let few = &all[..20];
    assert_eq!(
        urls(&Observation::select_comparable(few, 24)),
        urls(&select_frames(few, 24))
    );

    // Rows with no band centre at all are singleton buckets -> spread.
    let unknown_wave: Vec<Observation> = (0..10)
        .map(|i| Observation {
            em_min_um: f64::NAN,
            em_max_um: f64::NAN,
            ..obs(
                Some((i % 6 + 1) as u8),
                2.0,
                61000.0 + i as f64,
                format!("u{i}"),
            )
        })
        .collect();
    assert_eq!(
        urls(&Observation::select_comparable(&unknown_wave, 5)),
        urls(&select_frames(&unknown_wave, 5))
    );

    // Unknown-detector rows still bucket together by wavelength and can
    // win when nothing better exists.
    let mut mixed: Vec<Observation> = (0..25)
        .map(|i| {
            obs(
                Some((i % 6 + 1) as u8),
                1.0 + i as f64 * 0.5,
                61000.0 + i as f64,
                format!("https://stub.invalid/m/{i}.fits"),
            )
        })
        .collect();
    for i in 0..5 {
        mixed.push(obs(
            None,
            2.0,
            61000.0 + i as f64 * 10.0,
            format!("https://stub.invalid/nodet/{i}.fits"),
        ));
    }
    let sel = Observation::select_comparable(&mixed, 10);
    assert_eq!(sel.len(), 10);
    let nodet = sel.iter().filter(|o| o.detector_hint().is_none()).count();
    assert_eq!(nodet, 5, "the 5-epoch unknown-detector bucket ranks first");
}

// ---------------------------------------------------------------------
// API level: diff through the router with the stub source
// ---------------------------------------------------------------------

/// Stub scene for router tests: 24 epochs of detector 3 at 2.0 um
/// plus 36 rows of other detectors/wavelengths interleaved in time
/// (60 observations total, so `max_frames = 24` must select).
fn mixed_source() -> StubSource {
    let mut observations = Vec::new();
    for i in 0..24 {
        observations.push(obs(
            Some(3),
            2.0,
            61000.0 + i as f64 * 17.0,
            format!("https://stub.invalid/d3/{i}.fits"),
        ));
    }
    for i in 0..36 {
        observations.push(obs(
            Some((i % 6 + 1) as u8),
            1.0 + 0.4 * (i % 4) as f64,
            61005.0 + i as f64 * 11.0,
            format!("https://stub.invalid/other/{i}.fits"),
        ));
    }
    let mut frames = HashMap::new();
    for (i, o) in observations.iter().enumerate() {
        let (det, wave) = if i < 24 {
            (3, 2.0)
        } else {
            let j = i - 24;
            ((j % 6 + 1) as i64, 1.0 + 0.4 * (j % 4) as f64)
        };
        frames.insert(
            obs_id_hash(o),
            Arc::new(parse_frame(&cutout(1.0, det, wave as f32)).expect("fixture parses")),
        );
    }
    StubSource {
        observations,
        frames,
        ..StubSource::happy(0)
    }
}

#[tokio::test]
async fn api_diff_returns_comparable_frames() {
    let app = app(state(mixed_source()));
    let (status, _h, body) = call(
        &app,
        &Method::GET,
        "/api/v1/frames?ra=337.878&dec=-5.189&fov=0.05&mode=diff",
        &[],
    )
    .await
    .expect("request builds");
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).expect("json body");
    let frames = v["frames"].as_array().expect("frames array");
    // The 24 budget goes to the single 24-epoch D3 bucket -> 23 diffs
    // (before b13 the even spread diluted it below this bound).
    assert!(
        frames.len() >= 10,
        "diff returned {} frames, expected >= 10",
        frames.len()
    );
    for f in frames {
        assert_eq!(f["detector"], 3);
    }
    // Contract fields keep their meaning.
    assert_eq!(v["total_observations"], 60);
    assert_eq!(v["truncated"], true);
}

#[tokio::test]
async fn api_raw_keeps_the_even_spread() {
    let app = app(state(mixed_source()));
    let (status, _h, body) = call(
        &app,
        &Method::GET,
        "/api/v1/frames?ra=337.878&dec=-5.189&fov=0.05&mode=raw",
        &[],
    )
    .await
    .expect("request builds");
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).expect("json body");
    let frames = v["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 24);
    // Raw is the time-lapse: the spread still mixes detectors instead
    // of concentrating on the D3 bucket.
    let dets: std::collections::HashSet<u64> = frames
        .iter()
        .filter_map(|f| f["detector"].as_u64())
        .collect();
    assert!(dets.len() >= 2, "raw must stay spread, got {dets:?}");
}

// ---------------------------------------------------------------------
// Opt-in live check (never runs by default)
// ---------------------------------------------------------------------

/// Opt-in (`SPHEREX_LIVE=1`) selection-level before/after on real WISE
/// 0855 SIA rows: the largest comparable group the even spread picks
/// vs the one `select_comparable` picks. No cutouts are downloaded —
/// `search` issues one SIA query per configured collection.
#[tokio::test]
async fn live_wise0855_comparable_selection() {
    if std::env::var("SPHEREX_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let client = IrsaClient::from_env().expect("client from env");
    let obs = client
        .search(133.7597667, -7.2421833, 0.1)
        .await
        .expect("live search");
    let (spread_n, spread_base) = largest_bucket(&select_frames(&obs, 24));
    let (grouped_n, grouped_base) = largest_bucket(&Observation::select_comparable(&obs, 24));
    eprintln!(
        "WISE 0855: {} observations; even spread -> largest comparable \
         group {spread_n} epochs ({spread_base:.0} d baseline); \
         comparable selection -> {grouped_n} epochs ({grouped_base:.0} d)",
        obs.len()
    );
    assert!(
        grouped_n >= spread_n,
        "comparable selection must not be worse than the spread"
    );
}
