//! Task b8: scientific validation of the pipeline
//! `fits::parse_frame` -> `science::reproject` -> `group_frames` /
//! `subtract_epochs` / `coadd_epochs` / `stack_images` /
//! `extract_spectrum` -> `encode`, on synthetic SPHEREx-like cutouts
//! whose ground truth is known (`support/scenes.rs`).
//!
//! Ground-truth positions come from an INDEPENDENT TAN+SIP
//! implementation (`scenes::IndepWcs`), never from `src`'s own WCS code,
//! so these tests verify the pipeline instead of re-deriving its math.
//! All tests are deterministic (seeded RNG) and offline.

mod fixtures;
#[path = "support/scenes.rs"]
mod scenes;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ndarray::Array2;
use scenes::{flag_bit, IndepWcs, Rng, Scene, PIXEL_SCALE_DEG};
use spherex_backend::fits::{parse_frame, Frame};
use spherex_backend::science::*;

const RA0: f64 = 150.0;
const DEC0: f64 = 2.0;
const NIN: usize = 96;
const NOUT: usize = 64;
const PSF_FWHM: f32 = 2.0;
/// Required position accuracy, output pixels.
const TOL_PX: f64 = 0.1;

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

/// Common output grid: north up, east left, same scale as the scenes.
fn out_grid() -> OutputGrid {
    OutputGrid::centered(RA0, DEC0, NOUT as f64 * PIXEL_SCALE_DEG, NOUT).expect("valid output grid")
}

/// Independent inverse WCS of a (SIP-free) `OutputGrid`.
fn grid_truth(g: &OutputGrid) -> IndepWcs {
    IndepWcs {
        crpix: g.wcs.crpix,
        crval: g.wcs.crval,
        cd: g.wcs.cd,
        sip_a: Vec::new(),
        sip_b: Vec::new(),
    }
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Flux-weighted centroid (1-based) inside the `|dx|, |dy| <= r` window
/// around `(cx, cy)`; negative pixels are clipped to zero weight.
fn centroid_window(im: &Array2<f32>, cx: f64, cy: f64, r: f64) -> Option<(f64, f64)> {
    let (ny, nx) = im.dim();
    let (mut sx, mut sy, mut den) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..ny {
        for x in 0..nx {
            let (px, py) = (x as f64 + 1.0, y as f64 + 1.0);
            let v = im[[y, x]];
            if v.is_finite() && (px - cx).abs() <= r && (py - cy).abs() <= r {
                let w = f64::from(v).max(0.0);
                sx += px * w;
                sy += py * w;
                den += w;
            }
        }
    }
    (den > 0.0).then_some((sx / den, sy / den))
}

/// 1-based position and value of the brightest finite pixel.
fn argmax(im: &Array2<f32>) -> ((f64, f64), f32) {
    let (ny, nx) = im.dim();
    let (mut best, mut bv) = ((0.0, 0.0), f32::NEG_INFINITY);
    for y in 0..ny {
        for x in 0..nx {
            let v = im[[y, x]];
            if v.is_finite() && v > bv {
                best = (x as f64 + 1.0, y as f64 + 1.0);
                bv = v;
            }
        }
    }
    (best, bv)
}

/// 1-based position and value of the faintest finite pixel.
fn argmin(im: &Array2<f32>) -> ((f64, f64), f32) {
    let (ny, nx) = im.dim();
    let (mut best, mut bv) = ((0.0, 0.0), f32::INFINITY);
    for y in 0..ny {
        for x in 0..nx {
            let v = im[[y, x]];
            if v.is_finite() && v < bv {
                best = (x as f64 + 1.0, y as f64 + 1.0);
                bv = v;
            }
        }
    }
    (best, bv)
}

/// RMS over the finite pixels of an image.
fn rms_finite(im: &Array2<f32>) -> f64 {
    let (mut s, mut n) = (0.0f64, 0u64);
    for v in im.iter() {
        if v.is_finite() {
            s += f64::from(*v) * f64::from(*v);
            n += 1;
        }
    }
    (s / n as f64).sqrt()
}

fn mean_std(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let mean = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|z| (z - mean) * (z - mean)).sum::<f64>() / n;
    (mean, var.sqrt())
}

/// Tilted (20 deg + SIP) detector-1 scene at the field centre, 1.10 um,
/// zodi plane included.
fn tilted_scene(mjd: f64) -> Scene {
    let mut s = Scene::new(NIN, NIN)
        .detector(1)
        .mjd(mjd)
        .rotation(20.0)
        .sip(true)
        .pointing(RA0, DEC0)
        .wave(1.10, 0.002, -0.001);
    s.set_variance(0.25);
    s.zodi_plane(2.0, 0.01, -0.005);
    s
}

// ---------------------------------------------------------------------
// Fixture sanity: the scenes carry realistic, parseable headers
// ---------------------------------------------------------------------

#[test]
fn scene_fixtures_carry_realistic_headers() {
    let s = tilted_scene(61000.0);
    let bytes = s.to_bytes();
    let f = parse_frame(&bytes).expect("scene parses");
    assert_eq!(f.header.detector, Some(1));
    assert_eq!(f.header.mjd_avg, Some(61000.0));
    assert!(f.header.mjd_obs.is_some() && f.header.mjd_end.is_some());
    assert_eq!(f.header.unit.as_deref(), Some("MJy / sr"));
    for name in ["TRANSIENT", "SATURATED", "HOT", "SOURCE", "HALO"] {
        assert!(
            f.header.flag_legend.values().any(|b| b.name == name),
            "legend missing {name}"
        );
    }
    // Wavelength varies across the array (LVF realism); centre exact.
    let (ny, nx) = f.image.dim();
    let w = &f.wavelength;
    let span = (w.wavelength_at(1.0, 1.0) - w.wavelength_at(nx as f64, ny as f64)).abs();
    assert!(
        span > 0.05,
        "wavelength should vary across the array: {span}"
    );
    assert!((w.wavelength_at(48.5, 48.5) - 1.10).abs() < 1e-6);
    // SIP round-trip: the parser's inverse lands back on the injected
    // truth positions (validates header write + SIP read/invert).
    for &(x, y) in &[(20.3, 30.7), (48.5, 48.5), (80.0, 10.0)] {
        let (ra, dec) = s.wcs.pixel_to_sky(x, y);
        let (px, py) = f.wcs.radec_to_pixel(ra, dec).expect("in tangent plane");
        assert!(
            (px - x).abs() < 1e-4 && (py - y).abs() < 1e-4,
            "SIP round-trip at ({x},{y}) -> ({px},{py})"
        );
    }
}

// ---------------------------------------------------------------------
// 1. Moving source (proper motion) on tilted SIP WCS
// ---------------------------------------------------------------------

#[test]
fn moving_source_dipole_positions_flux_and_encode() {
    let grid = out_grid();
    let truth = grid_truth(&grid);
    let opts = ReprojectOptions {
        interp: Interp::Bilinear,
        subtract_zodi: true,
    };
    let p0 = (48.0, 48.0); // 1-based input pixel
    let dir = (0.8f64, 0.6f64); // diagonal proper-motion direction
    for &shift in &[0.5f64, 1.0, 2.0] {
        let p2 = (p0.0 + dir.0 * shift, p0.1 + dir.1 * shift);
        let mut s1 = tilted_scene(61000.0);
        let mut s2 = tilted_scene(61001.0);
        s1.gaussian_at_pix(p0.0, p0.1, 100.0, PSF_FWHM);
        s2.gaussian_at_pix(p2.0, p2.1, 100.0, PSF_FWHM);
        let f1 = s1.to_frame();
        let f2 = s2.to_frame();
        let e1 = reproject(&f1, &grid, &opts).expect("reproject e1");
        let e2 = reproject(&f2, &grid, &opts).expect("reproject e2");

        // Injected truth positions in output pixels (independent WCS).
        let sky1 = s1.wcs.pixel_to_sky(p0.0, p0.1);
        let sky2 = s2.wcs.pixel_to_sky(p2.0, p2.1);
        let exp1 = truth.sky_to_pixel(sky1.0, sky1.1);
        let exp2 = truth.sky_to_pixel(sky2.0, sky2.1);

        let m1 = centroid_window(&e1.image, exp1.0, exp1.1, 4.0).expect("epoch1 centroid");
        let m2 = centroid_window(&e2.image, exp2.0, exp2.1, 4.0).expect("epoch2 centroid");
        let err1 = dist(m1, exp1);
        let err2 = dist(m2, exp2);
        assert!(
            err1 <= TOL_PX && err2 <= TOL_PX,
            "shift {shift}: epoch positions {m1:?}/{m2:?} vs truth {exp1:?}/{exp2:?}"
        );
        // Measured displacement vs injected (per axis and magnitude).
        let de = (exp2.0 - exp1.0, exp2.1 - exp1.1);
        let dm = (m2.0 - m1.0, m2.1 - m1.1);
        let disp_err = dist(dm, de);
        assert!(
            (dm.0 - de.0).abs() <= TOL_PX
                && (dm.1 - de.1).abs() <= TOL_PX
                && (dm.0.hypot(dm.1) - de.0.hypot(de.1)).abs() <= TOL_PX,
            "shift {shift}: displacement {dm:?} vs truth {de:?}"
        );

        let d = subtract_epochs(&e2, &e1, DEFAULT_WAVELENGTH_TOLERANCE_UM).expect("subtract");
        // Flux conservation: sum of the difference ~ 0 within 1% of the
        // injected source flux (image minus zodi = sky model).
        let flux: f64 = s1
            .image
            .iter()
            .zip(&s1.zodi)
            .map(|(a, z)| f64::from(a - z))
            .sum();
        let sum: f64 = d
            .image
            .iter()
            .filter(|v| v.is_finite())
            .map(|v| f64::from(*v))
            .sum();
        assert!(
            sum.abs() <= 0.01 * flux,
            "shift {shift}: sum(diff) {sum} vs 1% of injected {flux}"
        );
        // Dipole extrema sit on the injected positions. For shifts <= PSF
        // sigma the lobes are pulled toward each other (the difference of
        // two blended Gaussians peaks near +-sigma of the midpoint), so
        // the extrema check uses a physically-derived 1 px window — see
        // the handoff table for measured values.
        let (pk, pv) = argmax(&d.image);
        let (tk, tv) = argmin(&d.image);
        assert!(pv > 0.0 && tv < 0.0, "shift {shift}: signed dipole");
        assert!(
            dist(pk, exp2) <= 1.0 && dist(tk, exp1) <= 1.0,
            "shift {shift}: extrema {pk:?}/{tk:?} vs truth {exp2:?}/{exp1:?}"
        );
        assert!(d.zodi_subtracted);
        eprintln!(
            "moving shift={shift}: pos_err=({err1:.4},{err2:.4}) disp_err={disp_err:.4} \
             sum_diff={sum:.4} pk_off={:.3} tk_off={:.3}",
            dist(pk, exp2),
            dist(tk, exp1)
        );

        // Encode round-trip of the difference image.
        let raw = STANDARD
            .decode(image_to_b64(&d.image))
            .expect("b64 decodes");
        assert_eq!(raw, image_to_le_bytes(&d.image));
        assert_eq!(raw.len(), NOUT * NOUT * 4);
    }
}

#[test]
fn stationary_source_cancels_to_noise() {
    let mut rng = Rng::new(0xB8_0002);
    let sigma = 0.5f32;
    let exp_rms = f64::from(sigma) * 2.0f64.sqrt();

    // Case A: aligned TAN grid -> identity reprojection, exact noise.
    let base = || {
        let mut s = Scene::new(NIN, NIN)
            .detector(1)
            .mjd(61000.0)
            .pointing(RA0, DEC0)
            .wave(1.10, 0.002, -0.001);
        s.gaussian_at_pix(48.0, 48.0, 60.0, PSF_FWHM);
        s.set_variance(sigma * sigma);
        s.zodi_plane(2.0, 0.01, -0.005);
        s
    };
    let mut a = base();
    a.add_noise(&mut rng, sigma);
    let mut b = base();
    b.add_noise(&mut rng, sigma);
    let fa = a.to_frame();
    let fb = b.to_frame();
    let g = OutputGrid::from_frame(NIN, NIN, &fa.wcs);
    let ea = reproject(&fa, &g, &ReprojectOptions::default()).unwrap();
    let eb = reproject(&fb, &g, &ReprojectOptions::default()).unwrap();
    let d = subtract_epochs(&eb, &ea, DEFAULT_WAVELENGTH_TOLERANCE_UM).unwrap();
    let r = rms_finite(&d.image);
    assert!(
        (r - exp_rms).abs() / exp_rms < 0.08,
        "aligned rms {r} vs expected {exp_rms}"
    );

    // Case B: tilted SIP WCS -> bilinear resampling can only smooth the
    // noise, never amplify it; the stationary source still cancels.
    let grid = out_grid();
    let mut t1 = tilted_scene(61000.0);
    t1.gaussian_at_pix(48.0, 48.0, 60.0, PSF_FWHM);
    t1.set_variance(sigma * sigma);
    t1.add_noise(&mut rng, sigma);
    let mut t2 = tilted_scene(61001.0);
    t2.gaussian_at_pix(48.0, 48.0, 60.0, PSF_FWHM);
    t2.set_variance(sigma * sigma);
    t2.add_noise(&mut rng, sigma);
    let f1 = t1.to_frame();
    let f2 = t2.to_frame();
    let e1 = reproject(&f1, &grid, &ReprojectOptions::default()).unwrap();
    let e2 = reproject(&f2, &grid, &ReprojectOptions::default()).unwrap();
    let d2 = subtract_epochs(&e2, &e1, DEFAULT_WAVELENGTH_TOLERANCE_UM).unwrap();
    let r2 = rms_finite(&d2.image);
    let worst = d2
        .image
        .iter()
        .filter(|v| v.is_finite())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(
        r2 <= exp_rms * 1.10 && r2 >= 0.3 * exp_rms,
        "tilted rms {r2} outside noise band around {exp_rms}"
    );
    assert!(
        worst <= 8.0 * exp_rms as f32,
        "residual peak {worst} above noise"
    );
    eprintln!("stationary: aligned_rms={r:.4} tilted_rms={r2:.4} exp={exp_rms:.4}");
}

// ---------------------------------------------------------------------
// 2. Alignment across pointing offsets and rotations
// ---------------------------------------------------------------------

#[test]
fn pointing_offsets_and_rotations_align() {
    let grid = out_grid();
    let truth = grid_truth(&grid);
    // Star fixed on the sky, ~5 px off the reference optical axis.
    let base = Scene::new(NIN, NIN).pointing(RA0, DEC0);
    let (sra, sdec) = base.wcs.pixel_to_sky(49.0, 52.0);
    let exp = truth.sky_to_pixel(sra, sdec);
    // (rotation_deg, dx_px, dy_px): pointing offsets up to 3 px. The
    // (90, 3, 0.5) variant is deliberately a sampling tie: every output
    // pixel maps exactly onto an input-pixel boundary in one axis.
    let variants = [
        (0.0f64, 0.0f64, 0.0f64),
        (20.0, 2.5, -1.5),
        (45.0, -3.0, 2.0),
        (-30.0, 1.0, 3.0),
        (60.0, -2.8, -2.8),
        (180.0, -1.0, -2.0),
        (90.0, 3.0, 0.5),
    ];
    for interp in [Interp::Nearest, Interp::Bilinear] {
        let opts = ReprojectOptions {
            interp,
            subtract_zodi: true,
        };
        let mut positions = Vec::new();
        let mut max_err = 0.0f64;
        for &(rot, dx, dy) in &variants {
            let mut s = Scene::new(NIN, NIN)
                .detector(1)
                .mjd(61000.0)
                .rotation(rot)
                .sip(true)
                .pointing(RA0, DEC0)
                .pointing_offset_px(dx, dy)
                .wave(1.10, 0.002, -0.001);
            let (px, py) = s.gaussian_at_sky(sra, sdec, 80.0, 3.0);
            assert!(
                px > 5.0 && px < NIN as f64 - 5.0 && py > 5.0 && py < NIN as f64 - 5.0,
                "rot={rot} off=({dx},{dy}): star at ({px:.1},{py:.1}) too near edge"
            );
            s.set_variance(0.25);
            s.zodi_plane(2.0, 0.01, -0.005);
            let f = s.to_frame();
            let e = reproject(&f, &grid, &opts).expect("reproject");
            let m = centroid_window(&e.image, exp.0, exp.1, 5.0).expect("star centroid");
            positions.push(m);
            max_err = max_err.max(dist(m, exp));
            // Nearest preserves the input pixel lattice: on axis-aligned
            // pointings (rotation a multiple of 90 deg) the fractional
            // part of the offset is quantized away, so the physical
            // bound is 0.5 px, not 0.1. Off-axis rotations decorrelate
            // the sample fractions and nearest meets the 0.1 px goal.
            let axis_aligned = rot.rem_euclid(90.0) < 1e-9;
            let tol = if interp == Interp::Nearest && axis_aligned {
                0.55
            } else {
                TOL_PX
            };
            assert!(
                dist(m, exp) <= tol,
                "{interp:?} rot={rot} off=({dx},{dy}): {m:?} vs truth {exp:?}"
            );
        }
        // The 0.1 px spec applies where the mode can physically reach
        // it: all bilinear pointings and the off-axis nearest ones.
        let strict: Vec<(f64, f64)> = positions
            .iter()
            .zip(&variants)
            .filter(|(_, &(rot, _, _))| !(interp == Interp::Nearest && rot.rem_euclid(90.0) < 1e-9))
            .map(|(m, _)| *m)
            .collect();
        let mut spread = 0.0f64;
        let mut full_spread = 0.0f64;
        for i in 0..positions.len() {
            for j in (i + 1)..positions.len() {
                full_spread = full_spread.max(dist(positions[i], positions[j]));
            }
        }
        for i in 0..strict.len() {
            for j in (i + 1)..strict.len() {
                spread = spread.max(dist(strict[i], strict[j]));
            }
        }
        // Pairwise spread bound is consistent with the per-pointing
        // tolerance: bilinear positions each stay within 0.1 px of truth
        // -> 0.1 px spread; nearest's 0.1 px off-axis tolerance allows a
        // 2x spread.
        let spread_tol = if interp == Interp::Nearest {
            2.0 * TOL_PX
        } else {
            TOL_PX
        };
        assert!(
            spread <= spread_tol,
            "{interp:?}: pairwise spread {spread:.4} px vs {spread_tol}"
        );
        eprintln!(
            "align {interp:?}: max_abs_err={max_err:.4}px strict_spread={spread:.4}px \
             full_spread={full_spread:.4}px"
        );
    }
}

// ---------------------------------------------------------------------
// 3. Orientation: an "L" pattern is not mirrored (both CD det signs)
// ---------------------------------------------------------------------

#[test]
fn orientation_l_pattern_not_mirrored() {
    let grid = out_grid();
    let truth = grid_truth(&grid);
    let opts = ReprojectOptions {
        interp: Interp::Bilinear,
        subtract_zodi: false,
    };
    for &mirror in &[false, true] {
        let mut s = Scene::new(NIN, NIN)
            .detector(1)
            .mjd(61000.0)
            .rotation(20.0)
            .mirror(mirror)
            .sip(true)
            .pointing(RA0, DEC0)
            .wave(1.10, 0.002, -0.001);
        let det = s.wcs.cd[0][0] * s.wcs.cd[1][1] - s.wcs.cd[0][1] * s.wcs.cd[1][0];
        assert_eq!(
            det < 0.0,
            !mirror,
            "CD determinant sign for mirror={mirror}"
        );
        // An L: bright vertices at the corner and both arm ends, faint
        // ridge along the arms.
        let corner = (42.0, 42.0);
        let x_end = (54.0, 42.0);
        let y_end = (42.0, 54.0);
        for i in 0..=12usize {
            s.image[42 * NIN + 42 + i] += 15.0;
            s.image[(42 + i) * NIN + 42] += 15.0;
        }
        for (x, y, a) in [
            (corner.0, corner.1, 100.0f32),
            (x_end.0, x_end.1, 90.0),
            (y_end.0, y_end.1, 80.0),
        ] {
            s.gaussian_at_pix(x, y, a, PSF_FWHM);
        }
        s.set_variance(0.25);
        let f = s.to_frame();
        let e = reproject(&f, &grid, &opts).expect("reproject");
        for &(x, y) in &[corner, x_end, y_end] {
            let sky = s.wcs.pixel_to_sky(x, y);
            let exp = truth.sky_to_pixel(sky.0, sky.1);
            let m = centroid_window(&e.image, exp.0, exp.1, 3.0)
                .unwrap_or_else(|| panic!("mirror={mirror} vertex ({x},{y})"));
            // A mirrored implementation places the arm on the wrong
            // side, ~24 px away; 0.4 px allows centroid pull from the
            // faint arms plus interpolation.
            assert!(
                dist(m, exp) <= 0.4,
                "mirror={mirror} vertex ({x},{y}): {m:?} vs truth {exp:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// 4. Wavelength grouping + typed mismatch (never silent)
// ---------------------------------------------------------------------

#[test]
fn wavelength_grouping_and_mismatch_never_silent() {
    let mk = |det: Option<i64>, lam: f64, mjd: f64| {
        let mut s = Scene::new(48, 48)
            .mjd(mjd)
            .rotation(20.0)
            .sip(true)
            .pointing(RA0, DEC0)
            .wave(lam, 0.002, -0.001);
        s = if let Some(d) = det {
            s.detector(d)
        } else {
            s.no_detector()
        };
        s.fill(1.0);
        s.set_variance(0.09);
        s.to_frame()
    };
    let f_d1_a = mk(Some(1), 0.90, 61000.0);
    let f_d1_b = mk(Some(1), 1.10, 61001.0);
    let f_d1_c = mk(Some(1), 1.12, 61002.0);
    let f_d3_a = mk(Some(3), 0.90, 61000.5);
    let f_d3_b = mk(Some(3), 1.10, 61001.5);
    let f_nodet = mk(None, 1.10, 61003.0);

    // Sanity: the fixture's wavelength varies across the array while the
    // central value is the injected one.
    let (ny, nx) = f_d1_b.image.dim();
    let w = &f_d1_b.wavelength;
    // The parsed map tracks the injected plane gradient:
    // |(dldx + dldy) * (nx - 1)| = 0.047 um corner to corner.
    let span = (w.wavelength_at(1.0, 1.0) - w.wavelength_at(nx as f64, ny as f64)).abs();
    assert!(
        (span - 0.047).abs() < 0.005,
        "wavelength gradient across array: {span} um"
    );
    assert!((central_wavelength(&f_d1_b) - 1.10).abs() < 1e-6);

    let groups = group_frames(
        &[&f_d1_a, &f_d1_b, &f_d1_c, &f_d3_a, &f_d3_b, &f_nodet],
        DEFAULT_WAVELENGTH_TOLERANCE_UM,
    )
    .expect("group_frames");
    // det1: {0.9} and {1.1, 1.12}; det3: {0.9} and {1.1}. The
    // detector-less frame is skipped entirely.
    assert_eq!(groups.len(), 4, "{groups:?}");
    assert_eq!(groups.iter().map(|g| g.frames.len()).sum::<usize>(), 5);
    let find = |det: u8, lam: f64| {
        groups
            .iter()
            .find(|g| g.detector == det && (g.wavelength_um - lam).abs() < 0.03)
            .unwrap_or_else(|| panic!("missing group det={det} lam={lam}"))
    };
    assert_eq!(find(1, 0.90).frames.len(), 1);
    let cl = find(1, 1.10);
    assert_eq!(cl.frames.len(), 2);
    // Members sorted by epoch: 61001 (1.10) before 61002 (1.12).
    assert_eq!(cl.frames[0].header.mjd_avg, Some(61001.0));
    assert_eq!(cl.frames[1].header.mjd_avg, Some(61002.0));
    assert_eq!(find(3, 0.90).frames.len(), 1);
    assert_eq!(find(3, 1.10).frames.len(), 1);

    // Mismatched bands are never silently subtracted: typed error.
    let grid = out_grid();
    let opts = ReprojectOptions::default();
    let e_a = reproject(&f_d1_a, &grid, &opts).unwrap();
    let e_b = reproject(&f_d1_b, &grid, &opts).unwrap();
    let e_c = reproject(&f_d1_c, &grid, &opts).unwrap();
    let err = subtract_epochs(&e_b, &e_a, DEFAULT_WAVELENGTH_TOLERANCE_UM)
        .expect_err("0.90 vs 1.10 must not subtract");
    match err {
        ScienceError::WavelengthMismatch {
            wave_a_um,
            wave_b_um,
            tolerance_um,
        } => {
            assert!((wave_a_um - 1.10).abs() < 0.02, "wave_a {wave_a_um}");
            assert!((wave_b_um - 0.90).abs() < 0.02, "wave_b {wave_b_um}");
            assert_eq!(tolerance_um, DEFAULT_WAVELENGTH_TOLERANCE_UM);
            // The message names BOTH wavelengths.
            let msg = err.to_string();
            assert!(
                msg.contains(&format!("{wave_a_um:.4}"))
                    && msg.contains(&format!("{wave_b_um:.4}")),
                "message names both wavelengths: {msg}"
            );
        }
        other => panic!("expected WavelengthMismatch, got {other:?}"),
    }
    // Same refusal through co-addition.
    assert!(matches!(
        coadd_epochs(
            &e_b,
            &e_a,
            DEFAULT_WAVELENGTH_TOLERANCE_UM,
            &StackOptions::default()
        ),
        Err(ScienceError::WavelengthMismatch { .. })
    ));
    // Inside tolerance (1.10 vs 1.12, diff 0.02 < 0.05) it proceeds.
    assert!(subtract_epochs(&e_c, &e_b, DEFAULT_WAVELENGTH_TOLERANCE_UM).is_ok());
}

// ---------------------------------------------------------------------
// 5. Stacking: inverse-variance, bad-pixel weights, sqrt(N) SNR
// ---------------------------------------------------------------------

#[test]
fn stacking_inverse_variance_n_frames() {
    let vars_all = [0.30f32, 0.43, 0.56, 0.69, 0.82, 0.95];
    let mut grid_opt: Option<OutputGrid> = None;
    for n in 1..=6usize {
        let mut ims = Vec::new();
        for (i, &v) in vars_all.iter().take(n).enumerate() {
            let mut s = Scene::new(48, 48)
                .detector(1)
                .mjd(61000.0 + i as f64)
                .pointing(RA0, DEC0)
                .wave(1.10, 0.0, 0.0);
            s.fill(7.5);
            s.set_variance(v);
            let f = s.to_frame();
            let g = *grid_opt.get_or_insert_with(|| OutputGrid::from_frame(48, 48, &f.wcs));
            ims.push(
                reproject(
                    &f,
                    &g,
                    &ReprojectOptions {
                        interp: Interp::Bilinear,
                        subtract_zodi: false,
                    },
                )
                .unwrap(),
            );
        }
        let refs: Vec<&ScienceImage> = ims.iter().collect();
        let st = stack_images(&refs, &StackOptions::default()).unwrap();
        let exp_var = 1.0
            / vars_all[..n]
                .iter()
                .map(|v| 1.0 / f64::from(*v))
                .sum::<f64>();
        let mut max_rel = 0.0f64;
        for y in 0..48 {
            for x in 0..48 {
                let got = f64::from(st.variance[[y, x]]);
                max_rel = max_rel.max((got - exp_var).abs() / exp_var);
                assert!(
                    (f64::from(st.image[[y, x]]) - 7.5).abs() < 1e-5,
                    "N={n} ({x},{y}) image {}",
                    st.image[[y, x]]
                );
                assert_eq!(st.n_contrib[[y, x]], n as u16);
            }
        }
        assert!(
            max_rel <= 1e-6,
            "N={n}: variance rel err {max_rel} vs 1/sum(1/var)={exp_var}"
        );
        eprintln!("stack N={n}: var={exp_var:.6} max_rel_err={max_rel:.2e}");
    }
    // coadd_epochs of two same-wavelength epochs == stack of two.
    let mut s1 = Scene::new(16, 16).pointing(RA0, DEC0).wave(1.10, 0.0, 0.0);
    s1.fill(3.0);
    s1.set_variance(0.2);
    let mut s2 = Scene::new(16, 16).pointing(RA0, DEC0).wave(1.10, 0.0, 0.0);
    s2.fill(5.0);
    s2.set_variance(0.4);
    let f1 = s1.to_frame();
    let f2 = s2.to_frame();
    let g = OutputGrid::from_frame(16, 16, &f1.wcs);
    let o = ReprojectOptions {
        interp: Interp::Nearest,
        subtract_zodi: false,
    };
    let i1 = reproject(&f1, &g, &o).unwrap();
    let i2 = reproject(&f2, &g, &o).unwrap();
    let cd = coadd_epochs(
        &i1,
        &i2,
        DEFAULT_WAVELENGTH_TOLERANCE_UM,
        &StackOptions::default(),
    )
    .unwrap();
    let sk = stack_images(&[&i1, &i2], &StackOptions::default()).unwrap();
    for y in 0..16 {
        for x in 0..16 {
            assert_eq!(cd.image[[y, x]], sk.image[[y, x]]);
            assert!((cd.image[[y, x]] - (3.0 * 5.0 + 5.0 * 2.5) / 7.5).abs() < 1e-5);
        }
    }
}

#[test]
fn stacking_bad_pixels_get_zero_weight() {
    let nx = 16;
    let mk = |val: f32, var: f32| {
        let mut s = Scene::new(nx, nx)
            .detector(1)
            .mjd(61000.0)
            .pointing(RA0, DEC0)
            .wave(1.10, 0.0, 0.0);
        s.fill(val);
        s.set_variance(var);
        s
    };
    let mut a = mk(100.0, 1.0);
    let b = mk(4.0, 0.25);
    // Bad-pixel classes along row y=5 (0-based column x):
    a.image[5 * nx + 5] = f32::NAN; // NaN flux
    a.variance[5 * nx + 6] = f32::NAN; // NaN variance
    a.variance[5 * nx + 7] = 0.0; // zero variance
    a.variance[5 * nx + 8] = -1.0; // negative variance
    a.set_flag(9, 5, flag_bit("SATURATED"));
    a.set_flag(10, 5, flag_bit("HOT"));
    a.set_flag(11, 5, flag_bit("TRANSIENT")); // not in the bad mask
    let fa = a.to_frame();
    let fb = b.to_frame();
    let g = OutputGrid::from_frame(nx, nx, &fa.wcs);
    let o = ReprojectOptions {
        interp: Interp::Nearest,
        subtract_zodi: false,
    };
    let ea = reproject(&fa, &g, &o).unwrap();
    let eb = reproject(&fb, &g, &o).unwrap();
    // Bad mask built from legend NAMES, never hardcoded bits.
    let mask = mask_from_names(&fa.header.flag_legend, &["SATURATED", "HOT"]);
    assert_eq!(mask, (1 << flag_bit("SATURATED")) | (1 << flag_bit("HOT")));
    let st = stack_images(
        &[&ea, &eb],
        &StackOptions {
            bad_flags_mask: mask,
        },
    )
    .unwrap();
    // Rejected inputs contribute nothing: stack = b's values, n_contrib=1.
    for x in [5usize, 6, 7, 8, 9, 10] {
        assert_eq!(st.n_contrib[[5, x]], 1, "x={x}");
        assert_eq!(st.image[[5, x]], 4.0, "x={x}");
        assert_eq!(st.variance[[5, x]], 0.25);
    }
    // Rejection flags still surface on the output.
    assert_ne!(st.flags[[5, 9]] & (1i32 << flag_bit("SATURATED")), 0);
    assert_ne!(st.flags[[5, 10]] & (1i32 << flag_bit("HOT")), 0);
    // TRANSIENT is not in the bad mask -> both frames contribute.
    assert_eq!(st.n_contrib[[5, 11]], 2);
    // Weighted mean: (100/1.0 + 4/0.25) / (1 + 4) = 116/5 = 23.2.
    assert!((st.image[[5, 11]] - 23.2).abs() < 1e-5);
    assert_eq!(st.n_contrib[[0, 0]], 2);
    assert!((st.image[[0, 0]] - 23.2).abs() < 1e-5);
    // Default mask (all bits bad) rejects the TRANSIENT pixel too.
    let st_all = stack_images(&[&ea, &eb], &StackOptions::default()).unwrap();
    assert_eq!(st_all.n_contrib[[5, 11]], 1);
    assert_eq!(st_all.image[[5, 11]], 4.0);
}

#[test]
fn stacking_snr_improves_sqrt_n_monte_carlo() {
    let mut rng = Rng::new(0xB8_0003);
    let sigma = 0.4f32;
    let truth = 5.0f32;
    let mut grid_opt: Option<OutputGrid> = None;
    for n in 1..=6usize {
        let mut ims = Vec::new();
        for _ in 0..n {
            let mut s = Scene::new(48, 48)
                .detector(1)
                .mjd(61000.0)
                .pointing(RA0, DEC0)
                .wave(1.10, 0.0, 0.0);
            s.fill(truth);
            s.set_variance(sigma * sigma);
            s.add_noise(&mut rng, sigma);
            let f = s.to_frame();
            let g = *grid_opt.get_or_insert_with(|| OutputGrid::from_frame(48, 48, &f.wcs));
            ims.push(
                reproject(
                    &f,
                    &g,
                    &ReprojectOptions {
                        interp: Interp::Nearest,
                        subtract_zodi: false,
                    },
                )
                .unwrap(),
            );
        }
        let refs: Vec<&ScienceImage> = ims.iter().collect();
        let st = stack_images(&refs, &StackOptions::default()).unwrap();
        // Residual rms of the stack vs the sigma/sqrt(N) prediction.
        let (mut ss, mut cnt, mut vsum) = (0.0f64, 0u64, 0.0f64);
        for v in st.image.iter() {
            if v.is_finite() {
                let d = f64::from(*v - truth);
                ss += d * d;
                cnt += 1;
            }
        }
        for v in st.variance.iter() {
            if v.is_finite() {
                vsum += f64::from(*v);
            }
        }
        let rms = (ss / cnt as f64).sqrt();
        let expected = f64::from(sigma) / (n as f64).sqrt();
        let ratio = rms / expected;
        let var_mean = vsum / cnt as f64;
        assert!(
            (ratio - 1.0).abs() <= 0.10,
            "N={n}: SNR gain {ratio:.3} not sqrt(N) within 10%"
        );
        assert!(
            (var_mean - expected * expected) / (expected * expected) < 0.02,
            "N={n}: stacked variance {var_mean} vs {expected}^2"
        );
        eprintln!("mc N={n}: rms={rms:.4} exp={expected:.4} ratio={ratio:.3} var={var_mean:.5}");
    }
}

// ---------------------------------------------------------------------
// 6. Spectrum recovery across 40 wavelengths
// ---------------------------------------------------------------------

#[test]
fn spectrum_recovers_shape_and_sigma() {
    let (nx, ny) = (48usize, 48usize);
    let (px, py) = (17.0f64, 21.0f64); // aperture-centre pixel, 1-based
    let wcs = Scene::new(nx, ny)
        .rotation(15.0)
        .sip(true)
        .pointing(RA0, DEC0)
        .wcs;
    let (sra, sdec) = wcs.pixel_to_sky(px, py);
    // Injected spectral shape S(lambda): continuum + a dip near 1.9 um.
    let shape = |l: f64| 4.0 + 1.5 * (l - 1.5) - 1.2 * (-(l - 1.9).powi(2) / 0.02).exp();
    let nlam = 40usize;
    let mut rng = Rng::new(0xB8_0004);
    for ap in [1u32, 3, 5] {
        let half = (ap / 2) as i64;
        let (cx, cy) = (px.round() as i64, py.round() as i64);
        let mut zs = Vec::with_capacity(520);
        for _rep in 0..13u32 {
            let mut frames = Vec::with_capacity(nlam);
            let mut expect = Vec::with_capacity(nlam);
            for i in 0..nlam {
                let lam = 0.75 + i as f64 * (1.67 / 39.0);
                let mut s = Scene::new(nx, ny)
                    .detector((i % 6 + 1) as i64)
                    .mjd(61000.0 + i as f64)
                    .rotation(15.0)
                    .sip(true)
                    .pointing(RA0, DEC0)
                    .wave(lam, 0.002, -0.001);
                let sv = shape(lam) as f32;
                s.fill(sv);
                s.gaussian_at_pix(px, py, 0.4 * sv, 3.0);
                let model = s.image.clone(); // noiseless sky model
                s.zodi_plane(1.0, 0.01, -0.004);
                let sig = 0.25 + 0.005 * i as f32;
                s.set_variance(sig * sig);
                s.add_noise(&mut rng, sig);
                // Expected aperture mean over the in-bounds box.
                let (mut sum, mut cnt) = (0.0f64, 0u64);
                for iy in (cy - half)..=(cy + half) {
                    for ix in (cx - half)..=(cx + half) {
                        if (1..=nx as i64).contains(&ix) && (1..=ny as i64).contains(&iy) {
                            sum += f64::from(model[(iy - 1) as usize * nx + (ix - 1) as usize]);
                            cnt += 1;
                        }
                    }
                }
                expect.push(sum / cnt as f64);
                frames.push(s.to_frame());
            }
            let frefs: Vec<&Frame> = frames.iter().collect();
            let pts = extract_spectrum(&frefs, sra, sdec, ap, true).expect("extract_spectrum");
            assert_eq!(pts.len(), nlam, "ap={ap}: every frame contributes");
            assert!(
                pts.windows(2)
                    .all(|w| w[0].wavelength_um <= w[1].wavelength_um),
                "ap={ap}: points sorted by wavelength"
            );
            // lambda_i increases with i, so the sorted order matches the
            // injected order (detector cycles 1..6).
            for (k, p) in pts.iter().enumerate() {
                assert_eq!(p.detector, Some((k % 6 + 1) as u8));
                assert!(p.sigma.is_finite() && p.sigma > 0.0);
                assert!(p.flags.is_empty());
                zs.push((p.value - expect[k]) / p.sigma);
            }
        }
        let (mean, std) = mean_std(&zs);
        let max_z = zs.iter().fold(0.0f64, |m, z| m.max(z.abs()));
        eprintln!(
            "spectrum ap={ap}: n={} z_mean={mean:.4} z_std={std:.4} max|z|={max_z:.2}",
            zs.len()
        );
        assert!(mean.abs() <= 0.10, "ap={ap}: z mean {mean}");
        assert!((std - 1.0).abs() <= 0.10, "ap={ap}: z std {std}");
        assert!(max_z <= 5.0, "ap={ap}: outlier z {max_z}");
    }
}

#[test]
fn spectrum_edge_nan_and_outside_positions() {
    let (nx, ny) = (32usize, 32usize);
    let wcs = Scene::new(nx, ny).rotation(10.0).pointing(RA0, DEC0).wcs;
    let (ra_e, dec_e) = wcs.pixel_to_sky(2.0, 2.0); // near the corner
    let mk = |image: Vec<f32>| {
        let mut s = Scene::new(nx, ny)
            .detector(2)
            .mjd(61000.0)
            .rotation(10.0)
            .pointing(RA0, DEC0)
            .wave(1.30, 0.002, -0.001);
        s.image = image;
        s.set_variance(0.09);
        s.to_frame()
    };
    // Model: flat 5 + a Gaussian exactly on the edge pixel (2, 2).
    let mut s0 = Scene::new(nx, ny).rotation(10.0).pointing(RA0, DEC0);
    s0.fill(5.0);
    s0.gaussian_at_pix(2.0, 2.0, 20.0, 2.0);
    let model = s0.image.clone();
    let f = mk(model.clone());
    // Aperture 5 at (2,2): clipped to x,y in 1..=4 (16 px).
    let pts = extract_spectrum(&[&f], ra_e, dec_e, 5, false).unwrap();
    assert_eq!(pts.len(), 1, "edge position must still produce a point");
    let (mut sum, mut cnt) = (0.0f64, 0u64);
    for iy in 0..=4i64 {
        for ix in 0..=4i64 {
            if ix >= 1 && iy >= 1 {
                sum += f64::from(model[(iy as usize - 1) * nx + ix as usize - 1]);
                cnt += 1;
            }
        }
    }
    let exp = sum / cnt as f64;
    assert!(
        (pts[0].value - exp).abs() < 1e-5,
        "edge aperture: {} vs {exp}",
        pts[0].value
    );
    // sigma over the clipped 16-px aperture: sqrt(16*var)/16 with the
    // f32-stored variance (0.09f32 != 0.09 exactly).
    let exp_sig = (16.0f64 * f64::from(0.09f32)).sqrt() / 16.0;
    assert!(
        (pts[0].sigma - exp_sig).abs() < 1e-9,
        "edge sigma {} vs {exp_sig}",
        pts[0].sigma
    );
    // Aperture fully NaN -> frame skipped, no point.
    let mut nan_img = model.clone();
    for iy in 0..=4usize {
        for ix in 0..=4usize {
            nan_img[iy * nx + ix] = f32::NAN;
        }
    }
    let f_nan = mk(nan_img);
    assert!(
        extract_spectrum(&[&f_nan], ra_e, dec_e, 5, false)
            .unwrap()
            .is_empty(),
        "all-NaN aperture must be skipped"
    );
    // Partial NaN: value comes from the finite pixels only.
    let mut part = model.clone();
    part[0] = f32::NAN; // (1,1) inside the aperture
    part[nx + 1] = f32::NAN; // (2,2) itself
    let f_part = mk(part.clone());
    let pts_p = extract_spectrum(&[&f_part], ra_e, dec_e, 5, false).unwrap();
    assert_eq!(pts_p.len(), 1);
    let (mut sum_p, mut cnt_p) = (0.0f64, 0u64);
    for iy in 0..=4i64 {
        for ix in 0..=4i64 {
            if ix >= 1 && iy >= 1 {
                let v = part[(iy as usize - 1) * nx + ix as usize - 1];
                if v.is_finite() {
                    sum_p += f64::from(v);
                    cnt_p += 1;
                }
            }
        }
    }
    let exp_p = sum_p / cnt_p as f64;
    assert!(
        (pts_p[0].value - exp_p).abs() < 1e-5,
        "partial-NaN aperture: {} vs {exp_p}",
        pts_p[0].value
    );
    // Position outside the footprint -> frame skipped, no crash.
    assert!(extract_spectrum(&[&f], 10.0, -80.0, 5, false)
        .unwrap()
        .is_empty());
    // Corner pixel (1,1): aperture clips to 3x3, still fine.
    let (ra_c, dec_c) = wcs.pixel_to_sky(1.0, 1.0);
    let pts_c = extract_spectrum(&[&f], ra_c, dec_c, 5, false).unwrap();
    assert_eq!(pts_c.len(), 1);
}

// ---------------------------------------------------------------------
// 7. Zodiacal subtraction is exact and flagged
// ---------------------------------------------------------------------

#[test]
fn zodi_subtraction_is_exact_and_flagged() {
    let mut s = Scene::new(48, 48)
        .detector(1)
        .mjd(61000.0)
        .pointing(RA0, DEC0)
        .wave(1.10, 0.0, 0.0);
    s.fill(3.0);
    s.set_variance(0.10);
    s.zodi_plane(1.5, 0.02, -0.01); // tilted plane, added to IMAGE too
    let f = s.to_frame();
    let g = OutputGrid::from_frame(48, 48, &f.wcs);
    for interp in [Interp::Nearest, Interp::Bilinear] {
        let on = reproject(
            &f,
            &g,
            &ReprojectOptions {
                interp,
                subtract_zodi: true,
            },
        )
        .unwrap();
        let off = reproject(
            &f,
            &g,
            &ReprojectOptions {
                interp,
                subtract_zodi: false,
            },
        )
        .unwrap();
        for y in 0..48 {
            for x in 0..48 {
                let z = f.zodi[[y, x]];
                assert!(
                    (off.image[[y, x]] - on.image[[y, x]] - z).abs() <= 1e-5,
                    "{interp:?} ({x},{y}): off-on = {}",
                    off.image[[y, x]] - on.image[[y, x]]
                );
                // And the subtracted image equals the injected sky (3.0).
                assert!((on.image[[y, x]] - 3.0).abs() <= 1e-5);
            }
        }
        assert!(on.zodi_subtracted, "{interp:?}: flag set");
        assert!(!off.zodi_subtracted, "{interp:?}: flag clear");
    }
    // The flag propagates through epoch arithmetic: mixed inputs clear
    // it, matching inputs keep it.
    let opts_on = ReprojectOptions {
        interp: Interp::Bilinear,
        subtract_zodi: true,
    };
    let opts_off = ReprojectOptions {
        interp: Interp::Bilinear,
        subtract_zodi: false,
    };
    let a_on = reproject(&f, &g, &opts_on).unwrap();
    let b_on = reproject(&f, &g, &opts_on).unwrap();
    let b_off = reproject(&f, &g, &opts_off).unwrap();
    assert!(
        subtract_epochs(&a_on, &b_on, DEFAULT_WAVELENGTH_TOLERANCE_UM)
            .unwrap()
            .zodi_subtracted
    );
    assert!(
        !subtract_epochs(&a_on, &b_off, DEFAULT_WAVELENGTH_TOLERANCE_UM)
            .unwrap()
            .zodi_subtracted,
        "mixed zodi state must clear the flag"
    );
}
