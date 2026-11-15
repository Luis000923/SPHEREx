//! IVOA TAP fallback for [`crate::irsa::IrsaClient::search`].
//!
//! When SIA2 is unreachable (HTTP 5xx or transport failure after all
//! retries), the same cone search is answered by the IRSA TAP service
//! (`{IRSA_TAP_URL}/sync`, default `https://irsa.ipac.caltech.edu/TAP`,
//! verified live 2026-11-14 via `GET /TAP/availability`) through ADQL
//! against the CAOM2 tables `spherex.plane` and `spherex.artifact`, the
//! pattern documented in the IRSA "Spectral Image Cutouts" notebook:
//! <https://caltech-ipac.github.io/irsa-tutorials/spherex-cutouts/>.
//!
//! Column mapping onto the SIA [`Observation`]:
//!
//! | TAP column                       | `Observation` field                |
//! |----------------------------------|------------------------------------|
//! | `a.uri`                          | `access_url` (host-relative path   |
//! |                                  | joined to `IRSA_BASE_URL`)          |
//! | `p.time_bounds_lower/upper`      | `t_min_mjd`/`t_max_mjd` (unit d)   |
//! | `p.energy_bounds_lower/upper`    | `em_min_um`/`em_max_um` (unit m)   |
//! | `p.energy_bandpassname`          | `band_name`                        |
//! | `ibe/data/spherex/<release>/...` | `collection` = `spherex_<release>` |
//!
//! Differences vs SIA2, kept deliberately and documented in the b15
//! handoff:
//! - `CONTAINS(POINT, poly)` tests the query *point* only: IRSA TAP
//!   rejects `INTERSECTS` ("Invalid or unsupported ADQL query string",
//!   verified 2026-11-14), so footprint-vs-FOV overlap cannot be
//!   expressed. Rows covering the square's edge but missing its centre
//!   are missed; the FOV `radius` is not sent (kept for the cache key
//!   only). This is safe — returned rows always overlap the point.
//! - There is no collection column; the release is recovered from the
//!   `ibe/data/spherex/<release>/` artifact path and mapped to the
//!   configured `spherex_<release>` name (`_` also tries a `/`
//!   boundary, so `spherex_dr1_deep` matches `dr1_deep/` or
//!   `dr1/deep/`). Rows whose release is not configured are dropped.
//! - Only `a.producttype = 'science'` artifacts are selected (verified
//!   live: every artifact at the WISE 0855 position is `science`).

use super::error::IrsaError;
use super::sia::{em_to_um, time_to_mjd, Observation};
use super::votable::{parse_votable, Votable};

/// Common prefix of every SPHEREx artifact `uri` (verified live
/// 2026-11-14 on `spherex.artifact` at the WISE 0855 position).
const IBE_PREFIX: &str = "ibe/data/spherex/";

/// ADQL cone query: spectral-image artifacts whose plane footprint
/// covers `(ra_deg, dec_deg)`, oldest first. All interpolated values
/// are pre-validated finite `f64`s, so no quoting is needed.
pub fn cone_query(ra_deg: f64, dec_deg: f64) -> String {
    format!(
        "SELECT a.uri, p.time_bounds_lower, p.time_bounds_upper, \
         p.energy_bounds_lower, p.energy_bounds_upper, p.energy_bandpassname \
         FROM spherex.artifact a \
         JOIN spherex.plane p ON a.planeid = p.planeid \
         WHERE 1=CONTAINS(POINT('ICRS',{ra_deg},{dec_deg}), p.poly) \
         AND a.producttype='science' \
         ORDER BY p.time_bounds_lower"
    )
}

/// Query parameters for `GET {tap_url}/sync` (VOTable sync TAP).
/// Returned as owned strings so the ADQL can outlive the call site.
pub fn sync_params(ra_deg: f64, dec_deg: f64) -> [(String, String); 4] {
    [
        ("REQUEST".to_string(), "doQuery".to_string()),
        ("LANG".to_string(), "ADQL".to_string()),
        ("FORMAT".to_string(), "votable".to_string()),
        ("QUERY".to_string(), cone_query(ra_deg, dec_deg)),
    ]
}

/// Map a TAP VOTable to observations, one row per `science` artifact
/// covering the position. `base_url` is the IRSA origin the host-relative
/// `a.uri` paths are joined to (the test stub serves its own); rows
/// whose release is not in `collections` are dropped, and rows without
/// a usable `uri` are skipped like SIA rows without `access_url`.
pub fn observations_from_tap(
    xml: &str,
    base_url: &str,
    collections: &[String],
) -> Result<Vec<Observation>, IrsaError> {
    // A rejected query (unsupported ADQL, server-side error) answers
    // 200 with an INFO QUERY_STATUS=ERROR and no TABLE.
    if xml.contains("QUERY_STATUS\" value=\"ERROR") || xml.contains("QUERY_STATUS' value='ERROR") {
        return Err(IrsaError::Parse("TAP rejected the query".into()));
    }
    let tables = parse_votable(xml)?;
    // The results table carries a `uri` column; a service-metadata
    // RESOURCE would not.
    let table = tables
        .iter()
        .find(|t| t.column("uri").is_some())
        .or_else(|| tables.iter().find(|t| !t.fields.is_empty()));
    let Some(t) = table else {
        return Err(IrsaError::Parse("no data table in TAP VOTable".into()));
    };
    Ok(rows_to_observations(t, base_url, collections))
}

/// Row-by-row conversion; pure and testable.
fn rows_to_observations(t: &Votable, base_url: &str, collections: &[String]) -> Vec<Observation> {
    let c_uri = t.column("uri");
    let c_tmin = t.column("time_bounds_lower");
    let c_tmax = t.column("time_bounds_upper");
    let c_emin = t.column("energy_bounds_lower");
    let c_emax = t.column("energy_bounds_upper");
    let c_band = t.column("energy_bandpassname");
    let u_emin = t.unit("energy_bounds_lower");
    let u_emax = t.unit("energy_bounds_upper");
    let u_tmin = t.unit("time_bounds_lower");
    let u_tmax = t.unit("time_bounds_upper");

    let mut out = Vec::with_capacity(t.rows.len());
    for row in &t.rows {
        let Some(uri) = t.cell(row, c_uri) else {
            continue;
        };
        let Some(collection) = collection_of(uri, collections) else {
            continue;
        };
        out.push(Observation {
            t_min_mjd: parse_f64(t.cell(row, c_tmin))
                .map(|v| time_to_mjd(v, u_tmin))
                .unwrap_or(f64::NAN),
            t_max_mjd: parse_f64(t.cell(row, c_tmax))
                .map(|v| time_to_mjd(v, u_tmax))
                .unwrap_or(f64::NAN),
            em_min_um: parse_f64(t.cell(row, c_emin))
                .map(|v| em_to_um(v, u_emin))
                .unwrap_or(f64::NAN),
            em_max_um: parse_f64(t.cell(row, c_emax))
                .map(|v| em_to_um(v, u_emax))
                .unwrap_or(f64::NAN),
            band_name: t.cell(row, c_band).unwrap_or_default().to_string(),
            access_url: access_url(base_url, uri),
            collection: collection.to_string(),
        });
    }
    out
}

/// Parse an optional double cell; non-numeric and non-finite values
/// (VOTable `NaN` sentinels) become `None`.
fn parse_f64(cell: Option<&str>) -> Option<f64> {
    cell.and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Absolute cutout base URL for a host-relative artifact `uri`.
/// An already-absolute `scheme://` URI is passed through unchanged.
fn access_url(base_url: &str, uri: &str) -> String {
    if uri.contains("://") {
        uri.to_string()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            uri.trim_start_matches('/')
        )
    }
}

/// The artifact path after `ibe/data/spherex/` (e.g.
/// `dr1/level2/.../file.fits`), whether `uri` is host-relative (the
/// usual IRSA shape) or an absolute `scheme://host/...` URL.
fn artifact_path(uri: &str) -> Option<&str> {
    let trimmed = uri.trim();
    if let Some(rest) = trimmed.trim_start_matches('/').strip_prefix(IBE_PREFIX) {
        return Some(rest);
    }
    let (_, rest) = trimmed.split_once("://")?;
    rest.split_once('/')?.1.strip_prefix(IBE_PREFIX)
}

/// The configured collection an artifact `uri` belongs to, or `None`.
///
/// The release is the first path segment after `ibe/data/spherex/`
/// (`dr1`, `qr3`, ...). A configured `spherex_<rel>` matches `rel/`; `_`
/// in `rel` also tries a `/` boundary so `spherex_dr1_deep` matches both
/// `dr1_deep/...` and `dr1/deep/...` layouts. `..` segments are rejected
/// (the path is joined to the base URL for cutout downloads).
fn collection_of<'a>(uri: &str, collections: &'a [String]) -> Option<&'a str> {
    let rest = artifact_path(uri)?;
    if rest.split('/').any(|s| s == "..") {
        return None;
    }
    for c in collections {
        let rel = c.strip_prefix("spherex_").unwrap_or(c);
        if rel.is_empty() {
            continue;
        }
        if rest.starts_with(&format!("{rel}/"))
            || (rel.contains('_') && rest.starts_with(&format!("{}/", rel.replace('_', "/"))))
        {
            return Some(c);
        }
    }
    None
}
