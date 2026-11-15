//! Preset-target tests (task b11): the five official challenge targets
//! are stored at epoch 2026.0 so that high-proper-motion objects still
//! land inside the small preset fields of view at the SPHEREx survey
//! epochs (2025-2027).
//!
//! `ASTROMETRY` below keeps the catalogue constants queried from SIMBAD
//! (`basic` table, ICRS epoch J2000) on 2026-11-14 for each target, so
//! the stored epoch-2026.0 coordinates can be checked numerically by
//! re-propagating them here. No test touches the network.
//!
//! Sources: Gaia EDR3 (2020yCat.1350....0G) for Barnard's Star,
//! eps Indi B and R Draconis; Kirkpatrick et al. 2021
//! (2021ApJS..253....7K) for WISE 0855; Hipparcos new reduction
//! (van Leeuwen 2007, 2007A&A...474..653V) for RW Aurigae.

use std::collections::HashSet;

use spherex_backend::api::targets::{preset_targets, warm_targets};
use spherex_backend::api::validation::{validate_query_params, FOV_MAX_DEG, FOV_MIN_DEG};

/// Catalogue astrometry at ICRS epoch J2000 (SIMBAD `basic`).
struct Astrometry {
    /// Preset id this row belongs to.
    id: &'static str,
    /// RA at epoch J2000, degrees.
    ra_deg: f64,
    /// Declination at epoch J2000, degrees.
    dec_deg: f64,
    /// Parallax, milliarcseconds.
    plx_mas: f64,
    /// Proper motion in RA (already x cos dec), mas/yr.
    pmra_masyr: f64,
    /// Proper motion in Dec, mas/yr.
    pmdec_masyr: f64,
    /// Radial velocity, km/s. 0 when unmeasured (eps Indi B, WISE 0855):
    /// its effect on the propagated direction is < 0.05 arcsec here.
    rv_kms: f64,
}

/// Epoch of the stored positions and the catalogue epoch offset.
const TARGET_EPOCH: f64 = 2026.0;
const CATALOGUE_EPOCH: f64 = 2000.0;

/// SIMBAD `basic` constants queried 2026-11-14 (see module doc).
const ASTROMETRY: &[Astrometry] = &[
    Astrometry {
        id: "barnards-star",
        ra_deg: 269.4520769586187,
        dec_deg: 4.693364966576667,
        plx_mas: 546.9759,
        pmra_masyr: -801.551,
        pmdec_masyr: 10362.394,
        rv_kms: -110.11,
    },
    Astrometry {
        id: "epsilon-indi-b",
        ra_deg: 331.04414658852124,
        dec_deg: -56.782844195116105,
        plx_mas: 270.658,
        pmra_masyr: 3981.977,
        pmdec_masyr: -2466.832,
        rv_kms: 0.0,
    },
    Astrometry {
        id: "wise-0855",
        ra_deg: 133.81890825,
        dec_deg: -7.247048,
        plx_mas: 439.0,
        pmra_masyr: -8123.7,
        pmdec_masyr: 673.2,
        rv_kms: 0.0,
    },
    Astrometry {
        id: "rw-aurigae",
        ra_deg: 76.95652603458333,
        dec_deg: 30.401438173888888,
        plx_mas: 15.42,
        pmra_masyr: 7.23,
        pmdec_masyr: -20.33,
        rv_kms: 15.0,
    },
    Astrometry {
        id: "r-draconis",
        ra_deg: 248.16761083295,
        dec_deg: 66.75497941722,
        plx_mas: 1.2509,
        pmra_masyr: -20.643,
        pmdec_masyr: 6.149,
        rv_kms: -132.8,
    },
];

/// km/s per (mas/yr of proper motion)/mas of parallax
/// (4.74047 = 1 AU in km, times 1/yr in s^-1 scaled to mas).
const AU_KM_S: f64 = 4.740470463533208;
/// One (km/s) * one Julian year, in parsec
/// (3.15576e7 s/yr / 3.08567758e13 km/pc).
const KMS_YR_TO_PC: f64 = 1.022712165e-6;
/// Propagation check tolerance, arcseconds.
const TOL_ARCSEC: f64 = 0.5;

/// Rigorous linear space motion: move the star along a straight 3-D line
/// at constant velocity for `dt` years, project back on the sphere
/// (standard formulation, e.g. astropy `apply_space_motion`).
/// Returns (ra_deg, dec_deg) at the new epoch.
fn propagate(a: &Astrometry, dt: f64) -> (f64, f64) {
    let (ra, dec) = (a.ra_deg.to_radians(), a.dec_deg.to_radians());
    let (sin_ra, cos_ra) = ra.sin_cos();
    let (sin_dec, cos_dec) = dec.sin_cos();
    let p = [cos_dec * cos_ra, cos_dec * sin_ra, sin_dec];
    let e_ra = [-sin_ra, cos_ra, 0.0];
    let e_dec = [-sin_dec * cos_ra, -sin_dec * sin_ra, cos_dec];
    let dist_pc = 1000.0 / a.plx_mas;
    let v_ra = AU_KM_S * a.pmra_masyr / a.plx_mas;
    let v_dec = AU_KM_S * a.pmdec_masyr / a.plx_mas;
    let r: Vec<f64> = (0..3)
        .map(|i| {
            let v_kms = a.rv_kms * p[i] + v_ra * e_ra[i] + v_dec * e_dec[i];
            dist_pc * p[i] + v_kms * KMS_YR_TO_PC * dt
        })
        .collect();
    let n = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
    (
        r[1].atan2(r[0]).to_degrees().rem_euclid(360.0),
        (r[2] / n).asin().to_degrees(),
    )
}

/// On-sky angular separation between two directions, arcseconds.
fn separation_arcsec(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let dra = (ra1 - ra2) * dec1.to_radians().cos();
    let ddec = dec1 - dec2;
    dra.hypot(ddec) * 3600.0
}

#[test]
fn presets_are_five_with_unique_ids() {
    let targets = preset_targets().expect("preset table parses");
    assert_eq!(targets.len(), 5);
    let ids: HashSet<&str> = targets.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids.len(), 5, "preset ids must be unique");
    for id in [
        "barnards-star",
        "epsilon-indi-b",
        "wise-0855",
        "rw-aurigae",
        "r-draconis",
    ] {
        assert!(ids.contains(id), "missing preset {id}");
    }
    for t in &targets {
        assert!(!t.name.is_empty(), "{}: empty name", t.id);
        assert!(t.note.is_some(), "{}: note must record the source", t.id);
    }
}

#[test]
fn preset_coordinates_pass_validation() {
    for t in preset_targets().expect("preset table parses") {
        validate_query_params(t.ra_deg, t.dec_deg, t.fov_deg)
            .unwrap_or_else(|e| panic!("{}: validate_query_params failed: {e:?}", t.id));
        assert!(
            (FOV_MIN_DEG..=FOV_MAX_DEG).contains(&t.fov_deg),
            "{}: fov {} outside [{FOV_MIN_DEG}, {FOV_MAX_DEG}]",
            t.id,
            t.fov_deg
        );
    }
}

#[test]
fn stored_positions_match_catalogue_propagated_to_2026() {
    let targets = preset_targets().expect("preset table parses");
    let dt = TARGET_EPOCH - CATALOGUE_EPOCH;
    for a in ASTROMETRY {
        let t = targets
            .iter()
            .find(|t| t.id == a.id)
            .unwrap_or_else(|| panic!("missing preset {}", a.id));
        let (ra, dec) = propagate(a, dt);
        let diff = separation_arcsec(ra, dec, t.ra_deg, t.dec_deg);
        assert!(
            diff <= TOL_ARCSEC,
            "{}: stored ({}, {}) is {diff:.3}\" from J2000+pm at \
             {TARGET_EPOCH} ({ra:.7}, {dec:.7}), exceeds {TOL_ARCSEC}\"",
            a.id,
            t.ra_deg,
            t.dec_deg
        );
    }
}

#[test]
fn warm_targets_use_the_same_epoch_2026_positions() {
    let targets = preset_targets().expect("preset table parses");
    let warm = warm_targets().expect("warm targets parse");
    assert_eq!(warm.len(), targets.len());
    for (w, t) in warm.iter().zip(targets.iter()) {
        assert_eq!(w.ra_deg, t.ra_deg, "{}: warm ra", t.id);
        assert_eq!(w.dec_deg, t.dec_deg, "{}: warm dec", t.id);
        assert_eq!(w.fov_deg, t.fov_deg, "{}: warm fov", t.id);
    }
}

/// Sanity bound on the naive tangent-plane propagation the task allowed
/// as a fallback: for these five targets it stays well under an arcsec
/// from the rigorous result over the 26-year span, so either method
/// would have produced the stored table.
#[test]
fn linear_propagation_is_close_to_rigorous_for_these_targets() {
    let dt = TARGET_EPOCH - CATALOGUE_EPOCH;
    for a in ASTROMETRY {
        let ra_lin = (a.ra_deg + a.pmra_masyr * 1e-3 * dt / a.dec_deg.to_radians().cos() / 3600.0)
            .rem_euclid(360.0);
        let dec_lin = a.dec_deg + a.pmdec_masyr * 1e-3 * dt / 3600.0;
        let (ra_rig, dec_rig) = propagate(a, dt);
        let diff = separation_arcsec(ra_lin, dec_lin, ra_rig, dec_rig);
        assert!(
            diff <= 1.0,
            "{}: linear vs rigorous propagation differ by {diff:.3}\"",
            a.id
        );
    }
}
