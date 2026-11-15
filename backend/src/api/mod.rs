//! HTTP API layer (task b5).
//!
//! Routes match `shared/openapi-contract.json` exactly:
//! `GET /api/v1/{health,targets,frames,spectrum}`. Unknown `/api/v1`
//! paths get a JSON `ErrorBody` 404, and so does every other `/api`
//! path (other versions, bare `/api`, the `/api/v1/` trailing slash);
//! everything else falls through to `STATIC_DIR` with an SPA fallback
//! to `index.html`.
//!
//! Middleware stack (outermost first):
//! - `Content-Security-Policy: frame-ancestors *` (embedding is a
//!   feature; `X-Frame-Options` is deliberately NOT set so it cannot
//!   deny framing);
//! - request tracing;
//! - strict CORS from `ALLOWED_ORIGINS` (GET only);
//! - gzip compression (`tower-http`);
//! - a request-timeout backstop;
//! - per-IP rate limiting on the API routes (`tower-governor`), which
//!   answers 429 with a contract `ErrorBody`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue, Method, Request};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tower::Service;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::warn;

use crate::config::Config;
use crate::error::AppError;
use crate::fits::Frame;
use crate::irsa::{IrsaClient, IrsaError, Observation, UpstreamStatus};

pub mod validation;

mod dto;
mod fetch;
mod frames;
mod health;
pub mod params;
mod spectrum;
pub mod targets;

/// Upstream frame provider: the slice of
/// [`crate::irsa::IrsaClient`] the API pipeline needs, abstracted so
/// tests can substitute a stub with synthetic fixtures (no network).
pub trait FrameSource: Send + Sync + 'static {
    /// SIA2 cone search merged over the configured collections,
    /// deduplicated by `access_url`.
    fn search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> impl Future<Output = Result<Arc<Vec<Observation>>, IrsaError>> + Send + '_;

    /// Download and decode one cutout for `observation`.
    fn fetch_cutout(
        &self,
        observation: Observation,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> impl Future<Output = Result<Arc<Frame>, IrsaError>> + Send + '_;

    /// Upstream reachability snapshot for `/api/v1/health` (b15).
    /// Cheap and non-blocking; the real [`IrsaClient`] may also spawn a
    /// detached probe at most once per `HEALTH_PROBE_SECS`. The default
    /// reports "no data yet" (`ok = true`, all timestamps `None`) so
    /// test stubs need no changes.
    fn upstream_health(&self) -> UpstreamStatus {
        UpstreamStatus::default()
    }

    /// Read-only coverage probe (b18): an already-cached cutout of
    /// `observation` whose box fully contains `(ra_deg, dec_deg)` plus
    /// an aperture margin of `margin_deg` degrees on the sky. Cache
    /// only — never upstream traffic; `None` means "fetch it". Default
    /// `None` keeps stubs that do not model a coverage cache compiling.
    fn cached_cutout(
        &self,
        observation: Observation,
        ra_deg: f64,
        dec_deg: f64,
        margin_deg: f64,
    ) -> impl Future<Output = Option<Arc<Frame>>> + Send + '_ {
        let _ = (observation, ra_deg, dec_deg, margin_deg);
        async { None }
    }

    /// Read-only coverage probe (b18): an already-cached observation
    /// list whose SIA search cone contains `(ra_deg, dec_deg)`. A cone
    /// containing the point is a superset of "observations covering the
    /// point" (a covering observation intersects the cone at the point
    /// itself). Cache only; `None` means "run the search".
    fn cached_observations(
        &self,
        ra_deg: f64,
        dec_deg: f64,
    ) -> impl Future<Output = Option<Arc<Vec<Observation>>>> + Send + '_ {
        let _ = (ra_deg, dec_deg);
        async { None }
    }
}

impl FrameSource for IrsaClient {
    fn search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> impl Future<Output = Result<Arc<Vec<Observation>>, IrsaError>> + Send + '_ {
        IrsaClient::search(self, ra_deg, dec_deg, fov_deg)
    }

    async fn fetch_cutout(
        &self,
        observation: Observation,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Arc<Frame>, IrsaError> {
        IrsaClient::fetch_cutout(self, &observation, ra_deg, dec_deg, fov_deg).await
    }

    fn upstream_health(&self) -> UpstreamStatus {
        IrsaClient::upstream_status(self)
    }

    async fn cached_cutout(
        &self,
        observation: Observation,
        ra_deg: f64,
        dec_deg: f64,
        margin_deg: f64,
    ) -> Option<Arc<Frame>> {
        self.cached_cutout_covering(&observation, ra_deg, dec_deg, margin_deg)
            .await
    }

    async fn cached_observations(
        &self,
        ra_deg: f64,
        dec_deg: f64,
    ) -> Option<Arc<Vec<Observation>>> {
        self.cached_observations_covering(ra_deg, dec_deg).await
    }
}

/// State shared by every handler; cheap to clone (`Arc`/`Copy` fields).
///
/// `Clone` is manual so `S` itself need not be `Clone` (the `Arc`
/// already shares it).
pub struct AppState<S> {
    /// Upstream frame provider.
    pub source: Arc<S>,
    /// SIA2 collections in use (reported by `/health`).
    pub collections: Arc<Vec<String>>,
    /// Directory with the built frontend (`STATIC_DIR`).
    pub static_dir: PathBuf,
    /// Strict CORS allow-list (`ALLOWED_ORIGINS`; empty = deny all
    /// cross-origin requests).
    pub allowed_origins: Arc<Vec<String>>,
    /// Side of the square output raster (`OUTPUT_NPIX`, <= 256).
    pub output_npix: usize,
    /// Hard cap on cutouts fetched per request (`MAX_FRAMES`).
    pub max_frames_cap: usize,
    /// Total upstream-fetch budget per request (`REQUEST_DEADLINE_SECS`).
    pub request_deadline: Duration,
    /// Answer early once enough frames decoded
    /// (`REQUEST_SOFT_DEADLINE_SECS`, see `fetch`).
    pub soft_deadline: Duration,
    /// Per-IP rate limit, requests/second (`RATE_LIMIT_RPS`).
    pub rate_limit_rps: u32,
}

impl<S> Clone for AppState<S> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            collections: self.collections.clone(),
            static_dir: self.static_dir.clone(),
            allowed_origins: self.allowed_origins.clone(),
            output_npix: self.output_npix,
            max_frames_cap: self.max_frames_cap,
            request_deadline: self.request_deadline,
            soft_deadline: self.soft_deadline,
            rate_limit_rps: self.rate_limit_rps,
        }
    }
}

impl<S> AppState<S> {
    /// Build state from a source and the process [`Config`].
    pub fn from_config(source: S, cfg: &Config) -> Self {
        Self {
            source: Arc::new(source),
            collections: Arc::new(cfg.irsa.collections.clone()),
            static_dir: cfg.static_dir.clone(),
            allowed_origins: Arc::new(cfg.allowed_origins.clone()),
            output_npix: cfg.output_npix,
            max_frames_cap: cfg.irsa.max_frames,
            request_deadline: cfg.request_deadline,
            soft_deadline: cfg.soft_deadline,
            rate_limit_rps: cfg.rate_limit_rps,
        }
    }
}

/// Assemble the complete router for `state`.
pub fn build_router<S: FrameSource>(state: AppState<S>) -> Router {
    // Per-IP rate limiting on the API only (static assets stay free).
    // SmartIpKeyExtractor honours the forwarded headers a trusted
    // reverse proxy sets and falls back to the peer IP (main always
    // serves with `ConnectInfo`).
    let mut governor = GovernorConfigBuilder::default().key_extractor(SmartIpKeyExtractor);
    governor
        .period(Duration::from_secs_f64(
            1.0 / f64::from(state.rate_limit_rps.max(1)),
        ))
        .burst_size(state.rate_limit_rps.max(1))
        .methods(vec![Method::GET])
        .error_handler(|e: GovernorError| match e {
            GovernorError::TooManyRequests { .. } => AppError::RateLimited.into_response(),
            _ => AppError::Internal.into_response(),
        });
    let governor_config = match governor.finish() {
        Some(c) => c,
        // `finish` fails only on a zero period or burst; both are
        // forced to >= 1 above.
        None => unreachable!("rate limiter period/burst are non-zero"),
    };

    let api = Router::new()
        .route("/health", get(health::get::<S>))
        .route("/targets", get(targets::get_targets))
        .route("/frames", get(frames::get_frames::<S>))
        .route("/spectrum", get(spectrum::get_spectrum::<S>))
        .fallback(api_fallback)
        .layer(GovernorLayer {
            config: Arc::new(governor_config),
        })
        .with_state(state.clone());

    let origins: Vec<HeaderValue> = state
        .allowed_origins
        .iter()
        .filter_map(|o| match HeaderValue::from_str(o) {
            Ok(v) => Some(v),
            Err(_) => {
                warn!("ignoring invalid ALLOWED_ORIGINS entry");
                None
            }
        })
        .collect();
    let cors = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_headers(Any)
        .allow_origin(AllowOrigin::list(origins));

    let spa = ServeDir::new(&state.static_dir)
        .fallback(ServeFile::new(state.static_dir.join("index.html")));

    Router::new()
        .nest("/api/v1", api)
        .fallback(move |req: Request<Body>| {
            let mut spa = spa.clone();
            async move {
                // Anything under `/api` outside the mounted `/api/v1`
                // tree (other versions, bare prefixes, the `/api/v1/`
                // trailing slash the nest does not match) gets the
                // contract JSON 404, never the SPA shell.
                let path = req.uri().path();
                if path == "/api" || path.starts_with("/api/") {
                    return AppError::NotFound.into_response();
                }
                match spa.call(req).await {
                    Ok(res) => res.map(Body::new),
                    Err(_) => AppError::Internal.into_response(),
                }
            }
        })
        // Backstop timeout: the in-handler deadline replies first.
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            state.request_deadline + Duration::from_secs(15),
        ))
        .layer(CompressionLayer::new().gzip(true))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors *"),
        ))
}

/// JSON 404 for anything under `/api/v1` that matched no route.
async fn api_fallback() -> AppError {
    AppError::NotFound
}
