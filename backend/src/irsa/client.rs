//! Async IRSA client: bounded concurrency, retries, size-capped
//! streaming downloads and a byte-weighted decoded-result cache.
//!
//! - Concurrency is bounded by a [`tokio::sync::Semaphore`] sized by
//!   `IRSA_MAX_CONCURRENCY` (default 3; IRSA reportedly answers 503 at
//!   4 concurrent cutout requests — see `docs/vault/decisions.md` D-005).
//! - Every request is retried on HTTP 503/429, timeouts and transport
//!   errors with exponential backoff + jitter, up to
//!   `IRSA_MAX_ATTEMPTS` (default 3) tries and a 15 s timeout.
//! - Bodies are streamed into memory only; anything beyond
//!   `IRSA_MAX_BODY_MB` (default 64 MiB) is rejected. No temp files.
//! - Decoded results (SIA observation lists and parsed FITS frames)
//!   live in a single `moka::future::Cache` weighted by bytes and
//!   capped at `MAX_CACHE_MB` (default 200 MiB).
//! - Concurrent identical requests are coalesced (`try_get_with`): one
//!   upstream download serves every waiter.
//! - Failures are negative-cached: deterministic ones (HTTP 4xx such as
//!   422 "Arrays do not overlap", undecodable FITS, oversize bodies) for
//!   [`PERMANENT_FAILURE_TTL`], transient ones (503 after all retries,
//!   timeouts) for `IRSA_FAILURE_TTL_SECS`. Without this a single bad
//!   observation was retried on every API call (~30 s each).

use std::sync::Arc;

use std::time::Duration;

use moka::future::Cache;
use moka::Expiry;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::api::validation::{
    DEC_MAX_DEG, DEC_MIN_DEG, FOV_MAX_DEG, FOV_MIN_DEG, RA_MAX_DEG, RA_MIN_DEG,
};
use crate::fits::subimage::{self, CutBox, HeaderBlock};
use crate::fits::{parse_frame, strip_extensions, FitsError, Frame};

use super::config::{CutoutSource, IrsaConfig};
use super::error::{backoff, Attempt, IrsaError};
use super::sia::{
    cutout_url, dedup_by_url, observations_from_votable, pos_parameter, search_radius_deg,
    select_frames, Observation,
};

/// A preset target to pre-warm the cache with (e.g. the five official
/// challenge targets at startup).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarmTarget {
    /// Right ascension, degrees.
    pub ra_deg: f64,
    /// Declination, degrees.
    pub dec_deg: f64,
    /// Field of view (square side), degrees.
    pub fov_deg: f64,
}

/// Outcome of [`IrsaClient::warm`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WarmReport {
    /// Targets whose frames were fetched and cached.
    pub succeeded: usize,
    /// Targets that failed (error logged, warming continues).
    pub failed: usize,
    /// Total frames cached across successful targets.
    pub frames: usize,
}

/// A parsed cutout together with the SIA row that produced it.
#[derive(Debug, Clone)]
pub struct CutoutFrame {
    /// The SIA2 row (times, spectral band, source collection).
    pub observation: Observation,
    /// Decoded MEF cutout (IMAGE/FLAGS/VARIANCE/ZODI/WCS-WAVE, WCS).
    /// Shared with the cache; cheap to clone.
    pub frame: Arc<Frame>,
}

/// Result of [`IrsaClient::fetch_frames`].
#[derive(Debug)]
pub struct FrameSet {
    /// Decoded frames, newest first.
    pub frames: Vec<CutoutFrame>,
    /// Observations covering the position before the `MAX_FRAMES` cap.
    pub total_observations: usize,
    /// True when `total_observations` exceeded the cap.
    pub truncated: bool,
}

/// Discriminates cache entries: SIA result lists vs decoded cutouts.
#[derive(Debug, Clone)]
enum Entry {
    /// Deduped observation list for one (position, radius).
    Observations(Arc<Vec<Observation>>),
    /// Decoded cutout for one (position, size, observation).
    Frame(Arc<Frame>),
}

#[derive(Hash, PartialEq, Eq, Clone)]
enum KeyKind {
    /// SIA cone search: (ra, dec, radius) as f64 bits.
    Search { ra: u64, dec: u64, radius: u64 },
    /// Cutout fetch: (ra, dec, size) as f64 bits plus observation id.
    Frame {
        ra: u64,
        dec: u64,
        size: u64,
        obs: u64,
    },
}

/// Cache key: (collection set, kind-specific tuple), per the task spec.
#[derive(Hash, PartialEq, Eq, Clone)]
struct CacheKey {
    /// Sorted, comma-joined collection set this client was built with.
    collections: Arc<str>,
    kind: KeyKind,
}

/// Byte weight of a cached entry (moka weigher). Frames dominate:
/// 4 arrays * 4 B/px plus a flat allowance for WCS-WAVE/WCS/strings.
fn entry_weight(e: &Entry) -> u32 {
    match e {
        Entry::Observations(obs) => {
            let bytes: usize = obs
                .iter()
                .map(|o| {
                    std::mem::size_of::<Observation>()
                        + o.band_name.len()
                        + o.access_url.len()
                        + o.collection.len()
                })
                .sum();
            u32::try_from(bytes).unwrap_or(u32::MAX)
        }
        Entry::Frame(f) => {
            let px = f.image.len() + f.variance.len() + f.zodi.len();
            let bytes = px.saturating_mul(4) + f.flags.len().saturating_mul(4) + 8192;
            u32::try_from(bytes).unwrap_or(u32::MAX)
        }
    }
}

/// Shared, cheaply cloneable client. All clones share the semaphore
/// and the cache.
/// Extensions dropped before a cutout is written to `CACHE_DIR`: the
/// cutout service copies the full `EPSF` table into every cutout
/// (3.86 of 3.96 MB measured on a real 0.05 deg cutout) and the backend
/// never decodes it.
const DISK_DROP_EXTENSIONS: &[&str] = &["EPSF"];

/// On-disk SIA results older than this are refetched (new ingests).
const SEARCH_DISK_TTL: Duration = Duration::from_secs(24 * 3600);

/// FNV-1a 64 over `s` (file names for the disk cache).
fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl CacheKey {
    /// Stable file name for this key inside `CACHE_DIR` (`suffix`
    /// distinguishes the per-collection SIA bodies of one search).
    fn file_name(&self, suffix: &str) -> String {
        match &self.kind {
            KeyKind::Search { ra, dec, radius } => format!(
                "search-{:016x}.xml",
                fnv64(&format!(
                    "s|{}|{ra}|{dec}|{radius}|{suffix}",
                    self.collections
                ))
            ),
            KeyKind::Frame { ra, dec, size, obs } => format!(
                "cutout-{:016x}.fits",
                fnv64(&format!("f|{}|{ra}|{dec}|{size}|{obs}", self.collections))
            ),
        }
    }
}

/// Read a disk-cache file; `None` when absent, unreadable or older than
/// `max_age`.
async fn disk_read(path: &std::path::Path, max_age: Option<Duration>) -> Option<Vec<u8>> {
    if let Some(age) = max_age {
        let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
        if modified.elapsed().map_or(true, |e| e > age) {
            return None;
        }
    }
    tokio::fs::read(path).await.ok()
}

/// Write a disk-cache file atomically (temp file + rename). Failures
/// are logged and ignored: the disk cache is an optimisation only.
async fn disk_write(dir: &std::path::Path, name: &str, bytes: &[u8]) {
    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        warn!("disk cache: cannot create directory: {e}");
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    if let Err(e) = tokio::fs::write(&tmp, bytes).await {
        warn!("disk cache: write failed: {e}");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, dir.join(name)).await {
        warn!("disk cache: rename failed: {e}");
        let _ = tokio::fs::remove_file(&tmp).await;
    }
}

/// How long deterministic upstream failures stay negative-cached.
pub const PERMANENT_FAILURE_TTL: Duration = Duration::from_secs(3600);

/// Cloneable record of a failed upstream fetch (negative cache value).
#[derive(Debug, Clone)]
enum Failure {
    /// Non-retryable HTTP status (4xx/5xx other than 503/429).
    Http(u16),
    /// Retries exhausted (503/429/timeouts/transport).
    Unavailable { attempts: usize, detail: String },
    /// Response could not be decoded (bad FITS / VOTable).
    Parse(String),
    /// Body over the size cap.
    TooLarge(usize),
}

impl Failure {
    /// Record for `e`, or `None` when it must not be cached (local
    /// misconfiguration or internal errors).
    fn of(e: &IrsaError) -> Option<Self> {
        match e {
            IrsaError::Http { status } => Some(Self::Http(*status)),
            IrsaError::Unavailable { attempts, detail } => Some(Self::Unavailable {
                attempts: *attempts,
                detail: detail.clone(),
            }),
            IrsaError::Parse(m) => Some(Self::Parse(m.clone())),
            IrsaError::Fits(f) => Some(Self::Parse(f.to_string())),
            IrsaError::BodyTooLarge { limit } => Some(Self::TooLarge(*limit)),
            IrsaError::InvalidQuery(_)
            | IrsaError::NoObservations
            | IrsaError::Config(_)
            | IrsaError::Internal(_) => None,
        }
    }

    fn to_error(&self) -> IrsaError {
        match self {
            Self::Http(status) => IrsaError::Http { status: *status },
            Self::Unavailable { attempts, detail } => IrsaError::Unavailable {
                attempts: *attempts,
                detail: detail.clone(),
            },
            Self::Parse(m) => IrsaError::Parse(m.clone()),
            Self::TooLarge(limit) => IrsaError::BodyTooLarge { limit: *limit },
        }
    }

    fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable { .. }) || matches!(self, Self::Http(s) if *s >= 500)
    }
}

/// Per-entry TTL for the negative cache.
struct FailureExpiry {
    transient: Duration,
}

impl Expiry<CacheKey, Failure> for FailureExpiry {
    fn expire_after_create(
        &self,
        _key: &CacheKey,
        value: &Failure,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(if value.is_transient() {
            self.transient
        } else {
            PERMANENT_FAILURE_TTL
        })
    }
}

/// Rebuild an owned error from a coalesced (shared) one.
fn unshare(e: &IrsaError) -> IrsaError {
    match Failure::of(e) {
        Some(f) => f.to_error(),
        None => match e {
            IrsaError::InvalidQuery(m) => IrsaError::InvalidQuery(m.clone()),
            IrsaError::NoObservations => IrsaError::NoObservations,
            IrsaError::Config(m) => IrsaError::Config(m.clone()),
            other => IrsaError::Internal(other.to_string()),
        },
    }
}

#[derive(Clone)]
pub struct IrsaClient {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: IrsaConfig,
    http: reqwest::Client,
    sem: Semaphore,
    /// Bounds range requests to the S3 mirror.
    s3_sem: Semaphore,
    collections_key: Arc<str>,
    cache: Cache<CacheKey, Entry>,
    failures: Cache<CacheKey, Failure>,
}

impl IrsaClient {
    /// Build a client from an explicit configuration.
    pub fn new(cfg: IrsaConfig) -> Result<Self, IrsaError> {
        cfg.validate()?;
        // `request_timeout` bounds connect and every idle gap while the
        // body streams, not the whole transfer: IRSA serves ~4 MB
        // cutouts at ~60 KB/s under load (66 s measured), and a total
        // timeout cut every one of them. `total_timeout` is the outer cap.
        let http = reqwest::Client::builder()
            .connect_timeout(cfg.request_timeout)
            .read_timeout(cfg.request_timeout)
            .timeout(cfg.total_timeout)
            .user_agent("spherex-sky-explorer/0.1 (NASA Space Apps 2026)")
            .build()
            .map_err(|e| IrsaError::Config(format!("cannot build HTTP client: {e}")))?;

        let mut collections = cfg.collections.clone();
        collections.sort();
        let collections_key: Arc<str> = Arc::from(collections.join(",").as_str());

        let cache = Cache::builder()
            .max_capacity(cfg.cache_max_mb.saturating_mul(1024 * 1024))
            .weigher(|_k: &CacheKey, v: &Entry| entry_weight(v))
            .build();
        let failures = Cache::builder()
            .max_capacity(10_000)
            .expire_after(FailureExpiry {
                transient: cfg.failure_ttl,
            })
            .build();

        Ok(Self {
            inner: Arc::new(Inner {
                sem: Semaphore::new(cfg.max_concurrency),
                s3_sem: Semaphore::new(cfg.s3_max_concurrency),
                http,
                collections_key,
                cache,
                failures,
                cfg,
            }),
        })
    }

    /// Build a client from `IRSA_*`/`MAX_*` environment variables.
    pub fn from_env() -> Result<Self, IrsaError> {
        Self::new(IrsaConfig::from_env()?)
    }

    /// The configuration this client was built with.
    pub fn config(&self) -> &IrsaConfig {
        &self.inner.cfg
    }

    /// Defensive validation (the API layer validates earlier; this
    /// guards `warm` and direct use). Bounds come from `api::validation`.
    fn check_query(&self, ra_deg: f64, dec_deg: f64, fov_deg: f64) -> Result<(), IrsaError> {
        for (name, v) in [("ra", ra_deg), ("dec", dec_deg), ("fov", fov_deg)] {
            if !v.is_finite() {
                return Err(IrsaError::InvalidQuery(format!(
                    "{name}: must be a finite number"
                )));
            }
        }
        if !(RA_MIN_DEG..RA_MAX_DEG).contains(&ra_deg) {
            return Err(IrsaError::InvalidQuery(
                "ra: out of range [0, 360) deg".into(),
            ));
        }
        if !(DEC_MIN_DEG..=DEC_MAX_DEG).contains(&dec_deg) {
            return Err(IrsaError::InvalidQuery(
                "dec: out of range [-90, 90] deg".into(),
            ));
        }
        if !(FOV_MIN_DEG..=FOV_MAX_DEG).contains(&fov_deg) {
            return Err(IrsaError::InvalidQuery(
                "fov: out of range [0.01, 0.5] deg".into(),
            ));
        }
        Ok(())
    }

    /// SIA2 cone search over every configured collection, deduped by
    /// `access_url`. Result is cached; an empty answer maps to
    /// [`IrsaError::NoObservations`] and is not cached.
    pub async fn search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Arc<Vec<Observation>>, IrsaError> {
        self.check_query(ra_deg, dec_deg, fov_deg)?;
        let radius = search_radius_deg(fov_deg);
        let key = CacheKey {
            collections: self.inner.collections_key.clone(),
            kind: KeyKind::Search {
                ra: ra_deg.to_bits(),
                dec: dec_deg.to_bits(),
                radius: radius.to_bits(),
            },
        };
        if let Some(Entry::Observations(hit)) = self.inner.cache.get(&key).await {
            debug!("irsa search cache hit");
            return Ok(hit);
        }
        if let Some(f) = self.inner.failures.get(&key).await {
            return Err(f.to_error());
        }

        let this = self.clone();
        let skey = key.clone();
        let fetched = self
            .inner
            .cache
            .try_get_with(key.clone(), async move {
                let sia = format!("{}/SIA", this.inner.cfg.base_url.trim_end_matches('/'));
                let pos = pos_parameter(ra_deg, dec_deg, radius);
                // Collections are queried concurrently (SIA took 20-33 s
                // per collection near Barnard's Star on 2026-11-14); the
                // shared semaphore still caps upstream concurrency.
                let mut set = JoinSet::new();
                for (idx, collection) in this.inner.cfg.collections.iter().enumerate() {
                    let (this, collection, sia, pos, skey) = (
                        this.clone(),
                        collection.clone(),
                        sia.clone(),
                        pos.clone(),
                        skey.clone(),
                    );
                    set.spawn(async move {
                        let body = this.sia_body(&sia, &collection, &pos, &skey).await?;
                        let text = String::from_utf8(body)
                            .map_err(|_| IrsaError::Parse("SIA response is not UTF-8".into()))?;
                        Ok::<_, IrsaError>((idx, observations_from_votable(&text, &collection)?))
                    });
                }
                let mut parts: Vec<(usize, Vec<Observation>)> = Vec::new();
                while let Some(joined) = set.join_next().await {
                    let part = joined.map_err(|e| {
                        IrsaError::Internal(format!(
                            "SIA task failed to join: {}",
                            join_error_kind(&e)
                        ))
                    })??;
                    parts.push(part);
                }
                // Keep the configured collection order (dedup: first wins).
                parts.sort_by_key(|(idx, _)| *idx);
                let all: Vec<Observation> = parts.into_iter().flat_map(|(_, o)| o).collect();
                let obs = Arc::new(dedup_by_url(all));
                if obs.is_empty() {
                    return Err(IrsaError::NoObservations);
                }
                Ok(Entry::Observations(obs))
            })
            .await;
        match fetched {
            Ok(Entry::Observations(obs)) => Ok(obs),
            Ok(Entry::Frame(_)) => Err(IrsaError::Internal("cache entry kind mismatch".into())),
            Err(e) => Err(self.remember_failure(key, &e).await),
        }
    }

    pub async fn fetch_cutout(
        &self,
        obs: &Observation,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Arc<Frame>, IrsaError> {
        self.check_query(ra_deg, dec_deg, fov_deg)?;
        let key = CacheKey {
            collections: self.inner.collections_key.clone(),
            kind: KeyKind::Frame {
                ra: ra_deg.to_bits(),
                dec: dec_deg.to_bits(),
                size: fov_deg.to_bits(),
                obs: obs.id_hash(),
            },
        };
        if let Some(Entry::Frame(hit)) = self.inner.cache.get(&key).await {
            return Ok(hit);
        }
        if let Some(f) = self.inner.failures.get(&key).await {
            return Err(f.to_error());
        }
        let url = cutout_url(&obs.access_url, ra_deg, dec_deg, fov_deg);
        let this = self.clone();
        let disk_key = key.clone();
        let obs_url = obs.access_url.clone();
        let fetched = self
            .inner
            .cache
            .try_get_with(key.clone(), async move {
                let disk = this
                    .inner
                    .cfg
                    .cache_dir
                    .as_ref()
                    .map(|d| (d.clone(), disk_key.file_name("")));
                if let Some((d, name)) = &disk {
                    if let Some(bytes) = disk_read(&d.join(name), None).await {
                        match parse_frame(&bytes) {
                            Ok(frame) => return Ok(Entry::Frame(Arc::new(frame))),
                            Err(e) => warn!(
                                "disk cache: discarding unreadable cutout: {}",
                                fits_error_kind(&e)
                            ),
                        }
                    }
                }
                let body = this
                    .cutout_bytes(&obs_url, &url, ra_deg, dec_deg, fov_deg)
                    .await?;
                let frame = parse_frame(&body)?;
                if let Some((d, name)) = &disk {
                    match strip_extensions(&body, DISK_DROP_EXTENSIONS) {
                        Ok(small) => disk_write(d, name, &small).await,
                        Err(e) => warn!("disk cache: cannot strip cutout: {}", fits_error_kind(&e)),
                    }
                }
                Ok::<_, IrsaError>(Entry::Frame(Arc::new(frame)))
            })
            .await;
        match fetched {
            Ok(Entry::Frame(frame)) => Ok(frame),
            Ok(Entry::Observations(_)) => {
                Err(IrsaError::Internal("cache entry kind mismatch".into()))
            }
            Err(e) => Err(self.remember_failure(key, &e).await),
        }
    }

    /// Raw SIA VOTable for one collection, from `CACHE_DIR` when fresh.
    async fn sia_body(
        &self,
        sia: &str,
        collection: &str,
        pos: &str,
        key: &CacheKey,
    ) -> Result<Vec<u8>, IrsaError> {
        let disk = self
            .inner
            .cfg
            .cache_dir
            .as_ref()
            .map(|d| (d.clone(), key.file_name(collection)));
        if let Some((d, name)) = &disk {
            if let Some(b) = disk_read(&d.join(name), Some(SEARCH_DISK_TTL)).await {
                return Ok(b);
            }
        }
        let b = self
            .get_with_retry(sia, &[("COLLECTION", collection), ("POS", pos)])
            .await?;
        if let Some((d, name)) = &disk {
            disk_write(d, name, &b).await;
        }
        Ok(b)
    }

    /// Negative-cache a (possibly shared) upstream failure and return
    /// an owned copy of it.
    async fn remember_failure(&self, key: CacheKey, e: &Arc<IrsaError>) -> IrsaError {
        if let Some(f) = Failure::of(e) {
            self.inner.failures.insert(key, f).await;
        }
        unshare(e)
    }

    pub async fn fetch_frames(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<FrameSet, IrsaError> {
        self.fetch_frames_n(ra_deg, dec_deg, fov_deg, self.inner.cfg.max_frames)
            .await
    }

    /// [`Self::fetch_frames`] with an explicit selection cap.
    pub async fn fetch_frames_n(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
        max_frames: usize,
    ) -> Result<FrameSet, IrsaError> {
        let observations = self.search(ra_deg, dec_deg, fov_deg).await?;
        let total = observations.len();
        let selected = select_frames(&observations, max_frames.min(self.inner.cfg.max_frames));
        let truncated = selected.len() < total;

        let mut set: JoinSet<(usize, Result<Arc<Frame>, IrsaError>)> = JoinSet::new();
        for (i, obs) in selected.iter().enumerate() {
            let inner = self.inner.clone();
            let obs = obs.clone();
            set.spawn(async move {
                let client = IrsaClient { inner };
                let res = client.fetch_cutout(&obs, ra_deg, dec_deg, fov_deg).await;
                (i, res)
            });
        }
        let mut slots: Vec<Option<Arc<Frame>>> = Vec::new();
        slots.resize_with(selected.len(), || None);
        let mut first_err = None;
        while let Some(joined) = set.join_next().await {
            let (i, res) = joined.map_err(|e| {
                IrsaError::Internal(format!(
                    "cutout task failed to join: {}",
                    join_error_kind(&e)
                ))
            })?;
            match res {
                Ok(frame) => slots[i] = Some(frame),
                Err(e) => {
                    // Never log the access URL (path leaks upstream
                    // layout); the observation id hash identifies it.
                    warn!(
                        "cutout for observation {} skipped: {}",
                        selected[i].id(),
                        e.log_safe()
                    );
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }

        let mut frames = Vec::new();
        for (obs, slot) in selected.into_iter().zip(slots) {
            if let Some(frame) = slot {
                frames.push(CutoutFrame {
                    observation: obs,
                    frame,
                });
            }
        }
        if frames.is_empty() {
            return Err(first_err.unwrap_or(IrsaError::NoObservations));
        }
        Ok(FrameSet {
            frames,
            total_observations: total,
            truncated,
        })
    }

    /// Pre-warm the cache for preset targets. Targets are processed
    /// sequentially (each `fetch_frames` already parallelizes its
    /// cutouts under the semaphore); failures are logged, not fatal.
    pub async fn warm(&self, targets: &[WarmTarget]) -> WarmReport {
        self.warm_with(targets, self.inner.cfg.max_frames).await
    }

    /// [`Self::warm`] selecting `max_frames` observations per target.
    /// Pass the API's default `max_frames` so the warmed cutouts are
    /// exactly the ones a default `/frames` request selects (warming
    /// with the 60-frame cap picked a different, mostly disjoint set).
    pub async fn warm_with(&self, targets: &[WarmTarget], max_frames: usize) -> WarmReport {
        let mut report = WarmReport::default();
        for t in targets {
            match self
                .fetch_frames_n(t.ra_deg, t.dec_deg, t.fov_deg, max_frames)
                .await
            {
                Ok(set) => {
                    report.succeeded += 1;
                    report.frames += set.frames.len();
                }
                Err(e) => {
                    warn!("cache warm failed for target {t:?}: {}", e.log_safe());
                    report.failed += 1;
                }
            }
        }
        report
    }

    /// One GET with the retry policy: bounded by the semaphore, retried
    /// on 503/429/timeouts/transport errors with exponential backoff +
    /// jitter, at most `cfg.max_attempts` tries.
    /// Bytes of the cutout of `access_url` around `(ra, dec)`: assembled
    /// from S3 byte ranges when configured and the file is mirrored,
    /// else (or when the mirror fails) from the IBE cutout `ibe_url`.
    /// A box that misses the array is HTTP 422 on both paths.
    async fn cutout_bytes(
        &self,
        access_url: &str,
        ibe_url: &str,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Vec<u8>, IrsaError> {
        if self.inner.cfg.cutout_source == CutoutSource::S3 {
            if let Some(key) = s3_key(access_url) {
                let url = format!("{}/{key}", self.inner.cfg.s3_base_url.trim_end_matches('/'));
                match self.cutout_from_s3(&url, ra_deg, dec_deg, fov_deg).await {
                    Ok(bytes) => return Ok(bytes),
                    Err(e @ IrsaError::Http { status: 422 }) => return Err(e),
                    Err(e) => {
                        warn!(
                            "S3 mirror cutout failed ({}); falling back to IBE",
                            e.log_safe()
                        )
                    }
                }
            }
        }
        self.get_with_retry(ibe_url, &[]).await
    }

    /// Rebuild the IBE cutout from ranges of the full-frame file at `url`.
    ///
    /// Pipelined to keep the number of sequential round trips small
    /// (~1-2 s each to the mirror measured from a home connection):
    /// the head (PRIMARY + IMAGE header) and the file tail (WCS-WAVE, via
    /// a suffix range) are read together; each of IMAGE/FLAGS/VARIANCE/
    /// ZODI then downloads its rows as soon as its header is known. EPSF
    /// is never read. Any layout surprise is an error, which makes the
    /// caller fall back to the IBE cutout service.
    async fn cutout_from_s3(
        &self,
        url: &str,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
    ) -> Result<Vec<u8>, IrsaError> {
        let (head, tail) = tokio::try_join!(
            self.get_range(url, 0..S3_HEAD_CHUNK),
            self.get_tail(url, S3_TAIL_CHUNK),
        )?;
        let primary = subimage::read_header(&head)?
            .ok_or_else(|| IrsaError::Parse("primary header exceeds the head chunk".into()))?;
        let image_off = primary.bytes.len() + primary.padded_data_len();
        let (image, image_at) = self.header_at(url, image_off, Some((0, &head))).await?;
        expect_ext(&image, "IMAGE")?;
        let Some(b) = subimage::cut_box(&image.header, ra_deg, dec_deg, fov_deg)? else {
            return Err(IrsaError::Http { status: 422 });
        };
        let wave = match find_wcs_wave(&tail) {
            Some(w) => w,
            None => {
                return Err(IrsaError::Parse(
                    "WCS-WAVE not found in the file tail".into(),
                ))
            }
        };

        let crop = |block: HeaderBlock, at: usize| async move {
            let r = subimage::row_range(&block.header, &b)?;
            let rows = self.get_range(url, at + r.start..at + r.end).await?;
            let data = subimage::crop_rows(&block.header, &rows, &b)?;
            Ok::<_, IrsaError>((subimage::cutout_header(&block, &b), data))
        };
        let flags_off = image_at + image.padded_data_len();
        let rest = async {
            let (flags, flags_at) = self.header_at(url, flags_off, None).await?;
            expect_ext(&flags, "FLAGS")?;
            let var_off = flags_at + flags.padded_data_len();
            let var_zodi = async {
                let (var, var_at) = self.header_at(url, var_off, None).await?;
                expect_ext(&var, "VARIANCE")?;
                let zodi_off = var_at + var.padded_data_len();
                let (var_hdu, (zodi, zodi_at)) = tokio::try_join!(
                    crop(var.clone(), var_at),
                    self.header_at(url, zodi_off, None)
                )?;
                expect_ext(&zodi, "ZODI")?;
                let zodi_hdu = crop(zodi, zodi_at).await?;
                Ok::<_, IrsaError>((var_hdu, zodi_hdu))
            };
            let (flags_hdu, (var_hdu, zodi_hdu)) =
                tokio::try_join!(self.flags_from_s3(url, &flags, flags_at, &b), var_zodi)?;
            Ok::<_, IrsaError>((flags_hdu, var_hdu, zodi_hdu))
        };
        let (img_hdu, (flags_hdu, var_hdu, zodi_hdu)) =
            tokio::try_join!(crop(image.clone(), image_at), rest)?;
        Ok(subimage::assemble(&[
            (primary.bytes.clone(), Vec::new()),
            img_hdu,
            flags_hdu,
            var_hdu,
            zodi_hdu,
            wave,
        ]))
    }

    /// Header block of the HDU starting at byte `off` and its data
    /// offset. `seed` is an already-read buffer `(start, bytes)` reused
    /// when it covers `off`; more bytes are read until the END card.
    async fn header_at(
        &self,
        url: &str,
        off: usize,
        seed: Option<(usize, &[u8])>,
    ) -> Result<(HeaderBlock, usize), IrsaError> {
        let mut buf = match seed {
            Some((start, bytes)) if off >= start && off < start + bytes.len() => {
                bytes[off - start..].to_vec()
            }
            _ => self.get_range(url, off..off + S3_HEADER_CHUNK).await?,
        };
        for _ in 0..8 {
            if let Some(block) = subimage::read_header(&buf)? {
                let at = off + block.bytes.len();
                return Ok((block, at));
            }
            let from = off + buf.len();
            let more = self.get_range(url, from..from + S3_HEADER_CHUNK).await?;
            if more.is_empty() {
                break;
            }
            buf.extend_from_slice(&more);
        }
        Err(IrsaError::Parse("FITS header without END".into()))
    }

    /// FLAGS HDU of the box: only the row-tiles it needs are downloaded
    /// and decoded (uncompressed FLAGS are cropped like the images).
    async fn flags_from_s3(
        &self,
        url: &str,
        block: &HeaderBlock,
        at: usize,
        b: &CutBox,
    ) -> Result<(Vec<u8>, Vec<u8>), IrsaError> {
        if block.header.opt_bool("ZIMAGE") != Some(true) {
            let r = subimage::row_range(&block.header, b)?;
            let rows = self.get_range(url, at + r.start..at + r.end).await?;
            let data = subimage::crop_rows(&block.header, &rows, b)?;
            return Ok((subimage::cutout_header(block, b), data));
        }
        let t = subimage::tile_rows(&block.header)?;
        let rows = self
            .get_range(url, at + b.y0 * t.row_len..at + (b.y0 + b.ny) * t.row_len)
            .await?;
        let descs = subimage::descriptors(&t, &rows)?;
        let lo = descs.iter().map(|d| d.1).min().unwrap_or(0);
        let hi = descs.iter().map(|d| d.1 + d.0).max().unwrap_or(0);
        let heap_at = at + t.heap_offset;
        let heap = self.get_range(url, heap_at + lo..heap_at + hi).await?;
        let px = subimage::decode_flag_rows(&block.header, &rows, &heap, lo, b)?;
        Ok((
            subimage::flags_image_header(block, b),
            subimage::i32_be(&px),
        ))
    }

    async fn get_with_retry(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<u8>, IrsaError> {
        self.get_retry_on(&self.inner.sem, url, query, None).await
    }

    /// GET the last `n` bytes of `url` from the S3 mirror (suffix range).
    async fn get_tail(&self, url: &str, n: usize) -> Result<Vec<u8>, IrsaError> {
        self.get_retry_on(&self.inner.s3_sem, url, &[], Some(ByteRange::Suffix(n)))
            .await
    }

    /// GET bytes `range` (half-open) of `url` from the S3 mirror.
    async fn get_range(
        &self,
        url: &str,
        range: std::ops::Range<usize>,
    ) -> Result<Vec<u8>, IrsaError> {
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let body = self
            .get_retry_on(
                &self.inner.s3_sem,
                url,
                &[],
                Some(ByteRange::Span(range.clone())),
            )
            .await?;
        if body.len() > range.len() {
            // Server ignored Range (200 with the full object).
            return Ok(body.get(range).map(<[u8]>::to_vec).unwrap_or_default());
        }
        Ok(body)
    }

    async fn get_retry_on(
        &self,
        sem: &Semaphore,
        url: &str,
        query: &[(&str, &str)],
        range: Option<ByteRange>,
    ) -> Result<Vec<u8>, IrsaError> {
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let outcome = {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|_| IrsaError::Internal("concurrency semaphore closed".into()))?;
                self.try_get(url, query, range.clone()).await
            };
            match outcome {
                Attempt::Ok(body) => return Ok(body),
                Attempt::Fatal(e) => return Err(e),
                Attempt::Retryable(detail) => {
                    if attempt >= self.inner.cfg.max_attempts {
                        return Err(IrsaError::Unavailable {
                            attempts: attempt,
                            detail,
                        });
                    }
                    let delay = backoff(self.inner.cfg.retry_base_delay, attempt);
                    debug!("irsa attempt {attempt} retryable ({detail}); sleeping {delay:?}");
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Single attempt: send, classify the status, stream the body into
    /// memory under the size cap.
    async fn try_get(
        &self,
        url: &str,
        query: &[(&str, &str)],
        range: Option<ByteRange>,
    ) -> Attempt {
        let mut req = self.inner.http.get(url).query(query);
        match &range {
            Some(ByteRange::Span(r)) => {
                req = req.header(
                    reqwest::header::RANGE,
                    format!("bytes={}-{}", r.start, r.end.saturating_sub(1)),
                );
            }
            Some(ByteRange::Suffix(n)) => {
                req = req.header(reqwest::header::RANGE, format!("bytes=-{n}"));
            }
            None => {}
        }
        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                // reqwest error strings embed the full URL (path +
                // query); record only the error class and the host.
                return Attempt::Retryable(transport_detail(&e));
            }
        };
        let status = resp.status();
        if status.as_u16() == 503 || status.as_u16() == 429 {
            return Attempt::Retryable(format!("HTTP {status}"));
        }
        if !status.is_success() {
            return Attempt::Fatal(IrsaError::Http {
                status: status.as_u16(),
            });
        }
        let cap = self.inner.cfg.max_body_bytes;
        if resp.content_length().is_some_and(|len| len > cap as u64) {
            return Attempt::Fatal(IrsaError::BodyTooLarge { limit: cap });
        }
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len().saturating_add(chunk.len()) > cap {
                        return Attempt::Fatal(IrsaError::BodyTooLarge { limit: cap });
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return Attempt::Retryable(transport_detail(&e)),
            }
        }
        Attempt::Ok(body)
    }
}

/// Bytes read for the head of a mirrored file (PRIMARY + IMAGE header:
/// 25 920 bytes in DR1 files).
const S3_HEAD_CHUNK: usize = 32 * 1024;
/// Bytes per extra header read.
const S3_HEADER_CHUNK: usize = 16 * 1024;
/// Bytes read from the end of a mirrored file to find WCS-WAVE (the last
/// HDU: 2880-byte header + 720-byte table in DR1). A multiple of 2880 so
/// block boundaries line up when the file length is a multiple of 2880.
const S3_TAIL_CHUNK: usize = 2880 * 8;

/// HTTP byte range of a request.
#[derive(Debug, Clone)]
enum ByteRange {
    /// Half-open `[start, end)`.
    Span(std::ops::Range<usize>),
    /// The last `n` bytes.
    Suffix(usize),
}

/// Error unless `block` is the extension `name`.
fn expect_ext(block: &HeaderBlock, name: &str) -> Result<(), IrsaError> {
    match block.extname() {
        Some(n) if n.eq_ignore_ascii_case(name) => Ok(()),
        other => Err(IrsaError::Parse(format!(
            "mirror layout: expected {name}, found {}",
            other.unwrap_or_else(|| "no EXTNAME".into())
        ))),
    }
}

/// Find the WCS-WAVE HDU (header + data) in the file tail `tail`, whose
/// last byte is the end of the file: scan 2880-byte block boundaries
/// counted from the end for an `XTENSION` header named WCS-WAVE whose
/// padded data ends exactly at the end of the file.
fn find_wcs_wave(tail: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    const BLOCK: usize = 2880;
    let n = tail.len();
    let mut end_off = BLOCK;
    while end_off <= n {
        let start = n - end_off;
        if tail[start..].starts_with(b"XTENSION") {
            if let Ok(Some(block)) = subimage::read_header(&tail[start..]) {
                let data_at = start + block.bytes.len();
                if block
                    .extname()
                    .is_some_and(|e| e.eq_ignore_ascii_case("WCS-WAVE"))
                    && data_at + block.padded_data_len() == n
                {
                    let data = tail[data_at..data_at + block.data_len].to_vec();
                    return Some((block.bytes, data));
                }
            }
        }
        end_off += BLOCK;
    }
    None
}

/// Object key of an IRSA `/ibe/data/spherex/<key>` access URL on the S3
/// mirror (the mirror uses the same keys, as SIA `cloud_access` shows).
pub(crate) fn s3_key(access_url: &str) -> Option<&str> {
    let (_, rest) = access_url.split_once("/ibe/data/spherex/")?;
    let key = rest.split(['?', '#']).next().unwrap_or(rest);
    (!key.is_empty() && !key.contains("..")).then_some(key)
}

/// Log/telemetry-safe description of a reqwest transport failure: the
/// error class plus the target host only. reqwest's `Display` embeds
/// the full URL ("error sending request for url (…)"), leaking the
/// upstream path and the `center`/`size` query into logs.
fn transport_detail(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect error"
    } else if e.is_body() || e.is_decode() {
        "body read error"
    } else {
        "request error"
    };
    match e.url().and_then(|u| u.host_str()) {
        Some(host) => format!("{kind} (host {host})"),
        None => kind.to_string(),
    }
}

/// Log-safe label for a failed task join (a panic payload can carry an
/// upstream URL; only the failure class is logged).
fn join_error_kind(e: &tokio::task::JoinError) -> &'static str {
    if e.is_panic() {
        "panic"
    } else if e.is_cancelled() {
        "cancelled"
    } else {
        "join failure"
    }
}

/// Log-safe label for a [`FitsError`]: the variant kind only (messages
/// can embed upstream-driven strings such as `EXTNAME`).
fn fits_error_kind(e: &FitsError) -> &'static str {
    match e {
        FitsError::NotFits(_) => "not a FITS file",
        FitsError::Truncated(_) => "truncated input",
        FitsError::Corrupt(_) => "corrupt content",
        FitsError::MissingKeyword(_) => "missing keyword",
        FitsError::MissingExtension { .. } => "missing extension",
        FitsError::BadData { .. } => "bad extension data",
        FitsError::UnsupportedCompression(_) => "unsupported compression",
        FitsError::UnsupportedProjection(_) => "unsupported projection",
        FitsError::Wcs(_) => "wcs failure",
    }
}
