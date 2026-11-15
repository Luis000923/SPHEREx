//! `GET /api/v1/health` (task b15): the contract `Health` fields plus
//! optional upstream-reachability fields. The contract schema sets no
//! `additionalProperties` on `Health`, so extras are legal JSON for the
//! endpoint — noted in the b15 handoff for the orchestrator.
//!
//! The handler stays cheap (the container healthcheck polls it):
//! `FrameSource::upstream_health` only reads outcomes the data client
//! already recorded and, for the real [`crate::irsa::IrsaClient`], may
//! spawn one *detached* probe per `HEALTH_PROBE_SECS` — it never blocks
//! the response on upstream I/O.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use super::dto::HealthBody;
use super::{AppState, FrameSource};

/// Optional `upstream` block appended to `Health` (b15).
#[derive(Debug, Serialize)]
struct UpstreamBlock {
    /// Unix seconds of the last successful SIA2 contact; `null` when
    /// SIA never succeeded since startup.
    last_sia_ok: Option<u64>,
    /// Unix seconds of the last successful TAP contact.
    last_tap_ok: Option<u64>,
    /// Unix seconds of the last successful IBE cutout contact.
    last_ibe_ok: Option<u64>,
    /// Unix seconds of the last successful S3-mirror contact.
    last_s3_ok: Option<u64>,
}

/// `Health` plus the optional b15 fields, flattened at top level.
#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    /// Contract fields: `status`, `version`, `collections`.
    #[serde(flatten)]
    base: HealthBody,
    /// `false` when the most recently finished upstream contact failed
    /// (5xx, timeout, retries exhausted); `true` on success and before
    /// any traffic — optimistic until evidence says otherwise.
    upstream_ok: bool,
    /// Per-channel last-success timestamps.
    upstream: UpstreamBlock,
}

/// `GET /api/v1/health`.
pub(crate) async fn get<S: FrameSource>(State(state): State<AppState<S>>) -> Json<HealthResponse> {
    let up = state.source.upstream_health();
    Json(HealthResponse {
        base: HealthBody {
            status: "ok",
            version: env!("CARGO_PKG_VERSION"),
            collections: (*state.collections).clone(),
        },
        upstream_ok: up.ok,
        upstream: UpstreamBlock {
            last_sia_ok: up.last_sia_ok,
            last_tap_ok: up.last_tap_ok,
            last_ibe_ok: up.last_ibe_ok,
            last_s3_ok: up.last_s3_ok,
        },
    })
}
