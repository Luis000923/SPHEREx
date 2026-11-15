//! Configuration for the IRSA client, loadable from environment.
//!
//! | Env var                | Default                              |
//! |------------------------|--------------------------------------|
//! | `IRSA_BASE_URL`        | `https://irsa.ipac.caltech.edu`      |
//! | `IRSA_COLLECTIONS`     | `spherex_dr1,spherex_qr3`            |
//! | `IRSA_MAX_CONCURRENCY` | `3` (IRSA reportedly 503s at 4)      |
//! | `MAX_FRAMES`           | `60`                                 |
//! | `MAX_CACHE_MB`         | `200`                                |
//! | `IRSA_TIMEOUT_SECS`    | `30` (connect + idle read)           |
//! | `IRSA_TOTAL_TIMEOUT_SECS` | `300` (whole transfer cap)        |
//! | `IRSA_FAILURE_TTL_SECS`| `60` (negative cache, transient)     |
//! | `CACHE_DIR`            | unset (memory only); see `client`    |
//! | `CUTOUT_SOURCE`        | `s3` (AWS mirror, IBE fallback) or `ibe` |
//! | `SPHEREX_S3_BASE_URL`  | `https://nasa-irsa-spherex.s3.us-east-1.amazonaws.com` |
//! | `S3_MAX_CONCURRENCY`   | `16`                                 |
//! | `IRSA_MAX_ATTEMPTS`    | `3`                                  |
//! | `IRSA_RETRY_BASE_MS`   | `400`                                |
//! | `IRSA_MAX_BODY_MB`     | `64`                                 |
//! | `SIA_FALLBACK_TAP`     | `1` (query TAP when SIA is down)   |
//! | `IRSA_TAP_URL`         | `https://irsa.ipac.caltech.edu/TAP` |
//! | `HEALTH_PROBE_SECS`    | `300` (min seconds between probes) |

use std::path::PathBuf;
use std::time::Duration;

use super::error::IrsaError;

/// Default IRSA origin.
pub const DEFAULT_BASE_URL: &str = "https://irsa.ipac.caltech.edu";
/// Default IVOA TAP endpoint (sync queries append `/sync`).
/// Verified live 2026-11-14 (`GET /TAP/availability`); the ADQL pattern
/// is documented in the IRSA cutouts notebook, see `irsa::tap`.
pub const DEFAULT_TAP_URL: &str = "https://irsa.ipac.caltech.edu/TAP";
/// Default SIA2 collections (`_deep` variants are opt-in via env).
/// Public AWS Open Data mirror of the SPHEREx files (same object keys as
/// the IRSA `/ibe/data/spherex/` paths; listed in SIA `cloud_access`).
pub const DEFAULT_S3_BASE_URL: &str = "https://nasa-irsa-spherex.s3.us-east-1.amazonaws.com";

/// Where cutouts come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoutSource {
    /// Byte ranges of the full-frame file on the AWS mirror, assembled
    /// locally (`fits::subimage`); falls back to IBE on failure.
    S3,
    /// The IRSA IBE cutout service only.
    Ibe,
}

pub const DEFAULT_COLLECTIONS: &[&str] = &["spherex_dr1", "spherex_qr3"];
/// Absolute cap on any IRSA response body kept in memory.
const DEFAULT_MAX_BODY: usize = 64 * 1024 * 1024;

/// Tuning knobs for [`crate::irsa::IrsaClient`].
#[derive(Debug, Clone)]
pub struct IrsaConfig {
    /// IRSA origin, e.g. `https://irsa.ipac.caltech.edu` (no trailing
    /// slash required; the SIA2 path `/SIA` is appended).
    pub base_url: String,
    /// SIA2 collections queried for every position, in priority order.
    /// Dedup across collections keeps the first hit per `access_url`.
    pub collections: Vec<String>,
    /// Bound on simultaneous outbound HTTP requests (semaphore).
    pub max_concurrency: usize,
    /// Maximum frames fetched per `fetch_frames` call.
    pub max_frames: usize,
    /// Total decoded-result cache budget, MiB.
    pub cache_max_mb: u64,
    /// Per-request HTTP timeout.
    pub request_timeout: Duration,
    /// How long a transient cutout/search failure (503 after retries,
    /// timeout) is remembered before the next upstream attempt.
    pub failure_ttl: Duration,
    /// Cap on one whole upstream transfer (`IRSA_TOTAL_TIMEOUT_SECS`);
    /// `request_timeout` is the connect/idle-read timeout.
    pub total_timeout: Duration,
    /// Total tries per request including the first (retries = n-1).
    pub max_attempts: usize,
    /// Base of the exponential backoff between retries.
    pub retry_base_delay: Duration,
    /// Reject response bodies larger than this many bytes.
    pub max_body_bytes: usize,
    /// Optional persistent cache directory (`CACHE_DIR`): SIA results
    /// and EPSF-stripped cutouts survive restarts. `None` = memory only.
    pub cache_dir: Option<PathBuf>,
    /// Cutout source (`CUTOUT_SOURCE`).
    pub cutout_source: CutoutSource,
    /// Base URL of the S3 mirror (`SPHEREX_S3_BASE_URL`).
    pub s3_base_url: String,
    /// Parallel range requests to the mirror (`S3_MAX_CONCURRENCY`).
    /// Separate from `IRSA_MAX_CONCURRENCY`: AWS is not the shared IRSA
    /// service.
    pub s3_max_concurrency: usize,
    /// When SIA fails transiently (HTTP 5xx, timeouts, retries
    /// exhausted), answer the search from TAP (`SIA_FALLBACK_TAP`,
    /// default on).
    pub sia_fallback_tap: bool,
    /// TAP endpoint base (`IRSA_TAP_URL`); `/sync` is appended.
    pub tap_url: String,
    /// Minimum spacing between upstream health probes triggered by
    /// `/api/v1/health` when nothing else reached IRSA recently
    /// (`HEALTH_PROBE_SECS`, default 300).
    pub health_probe_interval: Duration,
}

impl Default for IrsaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            collections: DEFAULT_COLLECTIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            max_concurrency: 3,
            max_frames: 60,
            cache_max_mb: 200,
            // 15 s cut real cutout downloads mid-body under IRSA load
            // (measured 2026-11-14, integration-live); 30 s lets them land.
            request_timeout: Duration::from_secs(30),
            failure_ttl: Duration::from_secs(60),
            total_timeout: Duration::from_secs(300),
            max_attempts: 3,
            retry_base_delay: Duration::from_millis(400),
            max_body_bytes: DEFAULT_MAX_BODY,
            cache_dir: None,
            cutout_source: CutoutSource::S3,
            s3_base_url: DEFAULT_S3_BASE_URL.to_string(),
            s3_max_concurrency: 16,
            sia_fallback_tap: true,
            tap_url: DEFAULT_TAP_URL.to_string(),
            health_probe_interval: Duration::from_secs(300),
        }
    }
}

impl IrsaConfig {
    /// Build from process environment, falling back to [`Self::default`]
    /// for unset variables. Returns [`IrsaError::Config`] on unparseable
    /// or inconsistent values.
    pub fn from_env() -> Result<Self, IrsaError> {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("IRSA_BASE_URL") {
            cfg.base_url = v;
        }
        if let Ok(v) = std::env::var("IRSA_COLLECTIONS") {
            cfg.collections = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        cfg.max_concurrency = env_usize("IRSA_MAX_CONCURRENCY", cfg.max_concurrency)?;
        cfg.max_frames = env_usize("MAX_FRAMES", cfg.max_frames)?;
        cfg.cache_max_mb = env_u64("MAX_CACHE_MB", cfg.cache_max_mb)?;
        cfg.request_timeout =
            Duration::from_secs(env_u64("IRSA_TIMEOUT_SECS", cfg.request_timeout.as_secs())?);
        if let Ok(v) = std::env::var("CUTOUT_SOURCE") {
            cfg.cutout_source = match v.trim().to_ascii_lowercase().as_str() {
                "s3" | "aws" => CutoutSource::S3,
                "ibe" | "irsa" => CutoutSource::Ibe,
                _ => return Err(IrsaError::Config("CUTOUT_SOURCE must be s3 or ibe".into())),
            };
        }
        if let Ok(v) = std::env::var("SPHEREX_S3_BASE_URL") {
            cfg.s3_base_url = v;
        }
        cfg.s3_max_concurrency = env_usize("S3_MAX_CONCURRENCY", cfg.s3_max_concurrency)?;
        cfg.cache_dir = std::env::var("CACHE_DIR")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        cfg.total_timeout = Duration::from_secs(env_u64(
            "IRSA_TOTAL_TIMEOUT_SECS",
            cfg.total_timeout.as_secs(),
        )?);
        cfg.failure_ttl =
            Duration::from_secs(env_u64("IRSA_FAILURE_TTL_SECS", cfg.failure_ttl.as_secs())?);
        cfg.max_attempts = env_usize("IRSA_MAX_ATTEMPTS", cfg.max_attempts)?;
        cfg.retry_base_delay = Duration::from_millis(env_u64(
            "IRSA_RETRY_BASE_MS",
            cfg.retry_base_delay.as_millis() as u64,
        )?);
        let body_mb = env_u64(
            "IRSA_MAX_BODY_MB",
            (cfg.max_body_bytes / 1024 / 1024) as u64,
        )?;
        cfg.max_body_bytes =
            usize::try_from(body_mb.saturating_mul(1024 * 1024)).unwrap_or(usize::MAX);
        if let Ok(v) = std::env::var("SIA_FALLBACK_TAP") {
            cfg.sia_fallback_tap = match v.trim() {
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" => true,
                "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF" => false,
                _ => {
                    return Err(IrsaError::Config(
                        "SIA_FALLBACK_TAP must be a boolean (1/0, true/false)".into(),
                    ))
                }
            };
        }
        if let Ok(v) = std::env::var("IRSA_TAP_URL") {
            cfg.tap_url = v;
        }
        cfg.health_probe_interval = Duration::from_secs(env_u64(
            "HEALTH_PROBE_SECS",
            cfg.health_probe_interval.as_secs(),
        )?);
        cfg.validate()?;
        Ok(cfg)
    }

    /// Enforce invariants; called by `from_env` and `IrsaClient::new`.
    pub fn validate(&self) -> Result<(), IrsaError> {
        if self.base_url.trim().is_empty() {
            return Err(IrsaError::Config("base URL is empty".into()));
        }
        if self.collections.is_empty() {
            return Err(IrsaError::Config(
                "IRSA_COLLECTIONS produced an empty list".into(),
            ));
        }
        if self.max_concurrency == 0 {
            return Err(IrsaError::Config(
                "IRSA_MAX_CONCURRENCY must be >= 1".into(),
            ));
        }
        if self.s3_max_concurrency == 0 {
            return Err(IrsaError::Config("S3_MAX_CONCURRENCY must be >= 1".into()));
        }
        if self.max_frames == 0 {
            return Err(IrsaError::Config("MAX_FRAMES must be >= 1".into()));
        }
        if self.max_attempts == 0 {
            return Err(IrsaError::Config("IRSA_MAX_ATTEMPTS must be >= 1".into()));
        }
        if self.max_body_bytes == 0 {
            return Err(IrsaError::Config("IRSA_MAX_BODY_MB must be >= 1".into()));
        }
        if self.sia_fallback_tap && self.tap_url.trim().is_empty() {
            return Err(IrsaError::Config(
                "IRSA_TAP_URL is empty while SIA_FALLBACK_TAP is on".into(),
            ));
        }
        Ok(())
    }
}

fn env_usize(name: &str, default: usize) -> Result<usize, IrsaError> {
    match std::env::var(name) {
        Ok(v) => v
            .parse::<usize>()
            .map_err(|_| IrsaError::Config(format!("{name} is not a non-negative integer"))),
        Err(_) => Ok(default),
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64, IrsaError> {
    match std::env::var(name) {
        Ok(v) => v
            .parse::<u64>()
            .map_err(|_| IrsaError::Config(format!("{name} is not a non-negative integer"))),
        Err(_) => Ok(default),
    }
}
