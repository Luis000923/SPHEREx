//! `GET /api/v1/targets`: the five official preset targets (task b5,
//! coordinates updated by task b11).
//!
//! The catalogue positions these presets used to hold are ICRS at epoch
//! J2000, but several of these objects have proper motions of arcseconds
//! per year, so at the SPHEREx survey epochs (2025-2027) they had moved
//! outside the small preset fields of view. The coordinates below are
//! the SIMBAD ICRS positions propagated to epoch 2026.0 with rigorous
//! 3-D space motion (parallax and radial velocity where measured); the
//! `note` of each preset records the J2000 catalogue position, the
//! proper motion and the source. Full provenance and the propagation
//! constants are in `docs/vault/handoffs/b11-presets-epoch.md` and in
//! `backend/tests/targets_tests.rs`.
//!
//! Values are kept in sync with `frontend/src/lib/presets.ts`. The
//! sexagesimal strings are parsed through the b1 grammar, so a typo in
//! this table fails loudly at first request (and in the unit test that
//! asserts the resolved degrees) instead of silently serving bad data.

use axum::Json;

use crate::api::validation::{parse_coordinates, validate_query_params};
use crate::error::AppError;
use crate::irsa::WarmTarget;

use super::dto::TargetDto;

/// One preset row (sexagesimal coordinates as in `presets.ts`), epoch 2026.0.
struct Preset {
    /// Stable machine id.
    id: &'static str,
    /// Display name (`label` in the frontend table).
    name: &'static str,
    /// RA, sexagesimal hours, epoch 2026.0.
    ra: &'static str,
    /// Dec, sexagesimal degrees, epoch 2026.0.
    dec: &'static str,
    /// Suggested field of view, degrees.
    fov_deg: f64,
    /// Optional human note.
    note: Option<&'static str>,
}

/// The five official challenge targets at epoch 2026.0; same values as
/// `frontend/src/lib/presets.ts`.
///
/// Sources (SIMBAD `basic`, ICRS epoch J2000 unless noted):
/// - Barnard's Star: Gaia EDR3, mu -801.55/+10362.39 mas/yr, plx 546.98 mas.
/// - epsilon Indi B: Gaia EDR3, mu +3981.98/-2466.83 mas/yr, plx 270.66 mas
///   (the Ba T1 component; the Ba/Bb pair is ~1.1 arcsec, unresolved).
/// - WISE 0855: Kirkpatrick et al. 2021, mu -8123.7/+673.2 mas/yr,
///   plx 439 mas. The challenge coordinates are the AllWISE ~2010.5 epoch.
/// - RW Aurigae: Hipparcos (van Leeuwen 2007), mu +7.23/-20.33 mas/yr.
/// - R Draconis: Gaia EDR3, mu -20.64/+6.15 mas/yr.
///
/// Fast movers get `fov_deg` 0.1 (+/-180 arcsec half-side) so the object
/// stays inside the field over the whole 2025-2027 survey span with
/// margin; the variables keep 0.05.
const PRESETS: &[Preset] = &[
    Preset {
        id: "barnards-star",
        name: "Barnard's Star",
        ra: "17h57m47.102s",
        dec: "+04d46m05.97s",
        fov_deg: 0.1,
        note: Some(
            "SIMBAD (Gaia EDR3) J2000: 269.45208, +4.69336, \
             pm -801.55/+10362.39 mas/yr; position propagated to epoch 2026.0",
        ),
    },
    Preset {
        id: "epsilon-indi-b",
        name: "Epsilon Indi B",
        ra: "22h04m23.201s",
        dec: "-56d48m02.34s",
        fov_deg: 0.1,
        note: Some(
            "Brown-dwarf binary Ba+Bb (~1.1 arcsec apart, unresolved at \
             6.15 arcsec/px). SIMBAD (Gaia EDR3) J2000, pm +3981.98/-2466.83 \
             mas/yr; propagated to epoch 2026.0",
        ),
    },
    Preset {
        id: "wise-0855",
        name: "WISE 0855",
        ra: "08h55m02.344s",
        dec: "-07d14m31.86s",
        fov_deg: 0.1,
        note: Some(
            "SIMBAD (Kirkpatrick et al. 2021) J2000: 133.81891, -7.24705, \
             pm -8123.7/+673.2 mas/yr; propagated to epoch 2026.0. The \
             challenge coordinates are the AllWISE ~2010.5 epoch",
        ),
    },
    Preset {
        id: "rw-aurigae",
        name: "RW Aurigae",
        ra: "05h07m49.581s",
        dec: "+30d24m04.65s",
        fov_deg: 0.05,
        note: Some(
            "SIMBAD (Hipparcos, van Leeuwen 2007) J2000: 76.95653, +30.40144, \
             pm +7.23/-20.33 mas/yr; propagated to epoch 2026.0",
        ),
    },
    Preset {
        id: "r-draconis",
        name: "R Draconis",
        ra: "16h32m40.136s",
        dec: "+66d45m18.09s",
        fov_deg: 0.05,
        note: Some(
            "SIMBAD (Gaia EDR3) J2000: 248.16761, +66.75498, \
             pm -20.64/+6.15 mas/yr; propagated to epoch 2026.0",
        ),
    },
];

/// Parsed presets as contract `Target`s. `Err` is unreachable (the
/// table is constants through the validated grammar) and maps to 500.
pub fn preset_targets() -> Result<Vec<TargetDto>, AppError> {
    PRESETS
        .iter()
        .map(|p| {
            let (ra_deg, dec_deg) = parse_coordinates(p.ra, p.dec)?;
            validate_query_params(ra_deg, dec_deg, p.fov_deg)?;
            Ok(TargetDto {
                id: p.id.to_string(),
                name: p.name.to_string(),
                ra_deg,
                dec_deg,
                fov_deg: p.fov_deg,
                note: p.note.map(str::to_string),
            })
        })
        .collect()
}

/// Presets as [`WarmTarget`]s for `IrsaClient::warm` (startup cache
/// pre-fetch, `WARM_TARGETS=1`).
pub fn warm_targets() -> Result<Vec<WarmTarget>, AppError> {
    Ok(preset_targets()?
        .iter()
        .map(|t| WarmTarget {
            ra_deg: t.ra_deg,
            dec_deg: t.dec_deg,
            fov_deg: t.fov_deg,
        })
        .collect())
}

/// `GET /api/v1/targets` handler.
pub(crate) async fn get_targets() -> Result<Json<Vec<TargetDto>>, AppError> {
    preset_targets().map(Json)
}
