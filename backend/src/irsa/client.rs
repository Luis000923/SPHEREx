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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use super::tap;

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

/// Geometry of one cached cutout, kept in the coverage index so a
/// later request can find "any cutout of this observation covering
/// this sky point" without scanning the whole cache.
#[derive(Clone)]
struct CutoutIndexEntry {
    /// Cutout centre requested upstream, degrees.
    ra: f64,
    /// Cutout centre requested upstream, degrees.
    dec: f64,
    /// Requested square side, degrees.
    size: f64,
    /// Key whose cache/disk entry holds the decoded frame.
    key: CacheKey,
}

/// Geometry of one cached SIA search cone in the coverage index.
#[derive(Clone)]
struct SearchIndexEntry {
    /// Cone centre, degrees.
    ra: f64,
    /// Cone centre, degrees.
    dec: f64,
    /// Cone radius, degrees.
    radius: f64,
    /// Key whose cache/disk entry holds the observation list.
    key: CacheKey,
}

/// Secondary index over the decoded-result cache (b18), guarded by a
/// std mutex (never held across `.await`). `cutouts` maps observation
/// id hash -> geometries of its cached cutouts; `searches` lists the
/// cached SIA cones. Lookups are O(entries of that observation /
/// remembered cones), not a full cache scan per click.
///
/// Bounds: at most [`INDEX_MAX_OBS`] distinct observations are tracked
/// (oldest bucket dropped FIFO), each with at most
/// [`INDEX_MAX_PER_OBS`] cutout geometries (oldest dropped FIFO), plus
/// [`INDEX_MAX_SEARCHES`] search cones (oldest dropped FIFO) — in all
/// `INDEX_MAX_OBS * INDEX_MAX_PER_OBS + INDEX_MAX_SEARCHES` small
/// entries (~64 B each, well under 10 MiB worst case). Entries are NOT
/// removed when moka evicts the value they point to: a stale entry
/// simply degrades to a `cache.get` miss (plus a disk probe when
/// `CACHE_DIR` is set) before the request falls back to the normal
/// fetch path.
#[derive(Default)]
struct CoverageIndex {
    /// Observation id hash -> geometries of its cached cutouts.
    cutouts: HashMap<u64, VecDeque<CutoutIndexEntry>>,
    /// Insertion order of `cutouts` keys, for the FIFO cap.
    obs_order: VecDeque<u64>,
    /// Cached SIA search cones, oldest first.
    searches: VecDeque<SearchIndexEntry>,
    /// Whether the `CACHE_DIR` sidecar files were scanned this process.
    disk_scanned: bool,
}

/// Distinct observations tracked by the coverage index.
const INDEX_MAX_OBS: usize = 1024;
/// Cutout geometries kept per observation.
const INDEX_MAX_PER_OBS: usize = 64;
/// SIA search cones remembered.
const INDEX_MAX_SEARCHES: usize = 512;

impl CoverageIndex {
    /// Record a cached cutout geometry (deduplicated by cache key).
    fn insert_cutout(&mut self, obs_hash: u64, entry: CutoutIndexEntry) {
        if !self.cutouts.contains_key(&obs_hash) {
            self.obs_order.push_back(obs_hash);
            while self.obs_order.len() > INDEX_MAX_OBS {
                if let Some(old) = self.obs_order.pop_front() {
                    self.cutouts.remove(&old);
                }
            }
        }
        let bucket = self.cutouts.entry(obs_hash).or_default();
        if bucket.iter().any(|e| e.key == entry.key) {
            return;
        }
        bucket.push_back(entry);
        while bucket.len() > INDEX_MAX_PER_OBS {
            bucket.pop_front();
        }
    }

    /// Record a cached SIA cone (deduplicated by cache key).
    fn insert_search(&mut self, entry: SearchIndexEntry) {
        if self.searches.iter().any(|e| e.key == entry.key) {
            return;
        }
        self.searches.push_back(entry);
        while self.searches.len() > INDEX_MAX_SEARCHES {
            self.searches.pop_front();
        }
    }

    /// Total tracked entries (diagnostic for tests).
    fn len(&self) -> usize {
        self.searches.len() + self.cutouts.values().map(VecDeque::len).sum::<usize>()
    }
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

    /// Sidecar file name carrying this key's coverage parameters next
    /// to its data file (`*.meta`), so a restarted process can rebuild
    /// the [`CoverageIndex`] from `CACHE_DIR` without reversing the
    /// hashed data-file names.
    fn meta_name(&self) -> String {
        match &self.kind {
            KeyKind::Search { ra, dec, radius } => format!(
                "search-{:016x}.meta",
                fnv64(&format!("m|{}|{ra}|{dec}|{radius}", self.collections))
            ),
            KeyKind::Frame { ra, dec, size, obs } => format!(
                "cutout-{:016x}.meta",
                fnv64(&format!("f|{}|{ra}|{dec}|{size}|{obs}", self.collections))
            ),
        }
    }

    /// Contents of the sidecar file: `v1 <kind> <collections> <params>`
    /// with f64 bits in hex so the values round-trip exactly.
    fn meta_body(&self) -> String {
        match &self.kind {
            KeyKind::Search { ra, dec, radius } => format!(
                "v1 search {} {ra:016x} {dec:016x} {radius:016x}",
                self.collections
            ),
            KeyKind::Frame { ra, dec, size, obs } => format!(
                "v1 cutout {} {ra:016x} {dec:016x} {size:016x} {obs:016x}",
                self.collections
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
/// Used for data files and for the `.meta` sidecars that document each
/// entry's coverage parameters (see [`CacheKey::meta_name`]).
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

/// Upstream service a request goes to; the health snapshot keeps the
/// last successful contact per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Upstream {
    /// SIA2 cone search (`{base}/SIA`).
    Sia,
    /// TAP sync query (`{tap}/sync`), the SIA fallback.
    Tap,
    /// IBE cutout download (`{access_url}?center=..&size=..`).
    Ibe,
    /// AWS S3 mirror range reads.
    S3,
}

/// Point-in-time upstream reachability report for `/api/v1/health`.
/// All timestamps are Unix seconds; `None` means the channel never
/// succeeded since startup.
#[derive(Debug, Clone, Copy)]
pub struct UpstreamStatus {
    /// `false` when the most recently finished upstream contact failed
    /// (5xx, timeout, retries exhausted); `true` on success and also
    /// before any traffic — optimistic until evidence says otherwise.
    pub ok: bool,
    /// Last successful SIA2 contact.
    pub last_sia_ok: Option<u64>,
    /// Last successful TAP contact.
    pub last_tap_ok: Option<u64>,
    /// Last successful IBE cutout contact.
    pub last_ibe_ok: Option<u64>,
    /// Last successful S3-mirror contact.
    pub last_s3_ok: Option<u64>,
}

impl Default for UpstreamStatus {
    fn default() -> Self {
        Self {
            ok: true,
            last_sia_ok: None,
            last_tap_ok: None,
            last_ibe_ok: None,
            last_s3_ok: None,
        }
    }
}

/// Recorded request outcomes feeding [`UpstreamStatus`]. The tracker
/// never performs I/O; recording happens where an upstream exchange
/// finishes (in `get_retry_on`).
#[derive(Debug, Default)]
struct Health {
    state: Mutex<HealthState>,
}

#[derive(Debug, Default)]
struct HealthState {
    last_sia_ok: Option<SystemTime>,
    last_tap_ok: Option<SystemTime>,
    last_ibe_ok: Option<SystemTime>,
    last_s3_ok: Option<SystemTime>,
    /// Outcome of the most recently finished upstream contact.
    last_ok: Option<bool>,
    /// When the last upstream contact finished (drives probe idleness).
    last_activity: Option<Instant>,
    /// When the last detached probe was started.
    probe_started: Option<Instant>,
}

fn unix_secs(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

impl Health {
    fn lock(&self) -> std::sync::MutexGuard<'_, HealthState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the final outcome of one upstream exchange. `ok` is the
    /// reachability verdict computed by the caller: a real HTTP answer
    /// below 500 counts as reachable even when it is a 4xx.
    fn record(&self, channel: Upstream, ok: bool) {
        let mut g = self.lock();
        g.last_activity = Some(Instant::now());
        g.last_ok = Some(ok);
        if ok {
            let slot = match channel {
                Upstream::Sia => &mut g.last_sia_ok,
                Upstream::Tap => &mut g.last_tap_ok,
                Upstream::Ibe => &mut g.last_ibe_ok,
                Upstream::S3 => &mut g.last_s3_ok,
            };
            *slot = Some(SystemTime::now());
        }
    }

    /// True at most once per `interval` and only when no upstream
    /// contact (real request or probe) finished inside the window —
    /// health probing adds zero traffic while the app is being used.
    fn probe_due(&self, interval: Duration) -> bool {
        let mut g = self.lock();
        let now = Instant::now();
        let idle = g
            .last_activity
            .is_none_or(|t| now.duration_since(t) >= interval);
        let not_running = g
            .probe_started
            .is_none_or(|t| now.duration_since(t) >= interval);
        if !(idle && not_running) {
            return false;
        }
        g.probe_started = Some(now);
        true
    }

    fn snapshot(&self) -> UpstreamStatus {
        let g = self.lock();
        UpstreamStatus {
            ok: g.last_ok.unwrap_or(true),
            last_sia_ok: unix_secs(g.last_sia_ok),
            last_tap_ok: unix_secs(g.last_tap_ok),
            last_ibe_ok: unix_secs(g.last_ibe_ok),
            last_s3_ok: unix_secs(g.last_s3_ok),
        }
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
    /// Upstream-reachability tracker for `/api/v1/health`.
    health: Health,
    /// Coverage index over `cache`/`CACHE_DIR` entries (b18).
    index: Mutex<CoverageIndex>,
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
                health: Health::default(),
                index: Mutex::new(CoverageIndex::default()),
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
            self.index_search(ra_deg, dec_deg, radius, key);
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
                let obs = this
                    .search_observations(ra_deg, dec_deg, radius, &skey)
                    .await?;
                if obs.is_empty() {
                    return Err(IrsaError::NoObservations);
                }
                Ok(Entry::Observations(Arc::new(obs)))
            })
            .await;
        match fetched {
            Ok(Entry::Observations(obs)) => {
                self.index_search(ra_deg, dec_deg, radius, key);
                Ok(obs)
            }
            Ok(Entry::Frame(_)) => Err(IrsaError::Internal("cache entry kind mismatch".into())),
            Err(e) => Err(self.remember_failure(key, &e).await),
        }
    }

    /// Observation list covering the position: SIA2 over every
    /// configured collection, or the TAP cone query when SIA fails
    /// transiently and `sia_fallback_tap` is on. Deduped by
    /// `access_url`; an empty list is *not* an error here (the caller
    /// maps it to [`IrsaError::NoObservations`]).
    async fn search_observations(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        radius_deg: f64,
        key: &CacheKey,
    ) -> Result<Vec<Observation>, IrsaError> {
        match self.sia_search(ra_deg, dec_deg, radius_deg, key).await {
            Ok(obs) => Ok(dedup_by_url(obs)),
            Err(e) => {
                if !self.inner.cfg.sia_fallback_tap || !transient_failure(&e) {
                    return Err(e);
                }
                warn!("SIA failed ({}); trying TAP fallback", e.log_safe());
                match self.tap_search(ra_deg, dec_deg, key).await {
                    Ok(obs) => Ok(dedup_by_url(obs)),
                    Err(tap_err) => {
                        warn!("TAP fallback failed too: {}", tap_err.log_safe());
                        Err(e)
                    }
                }
            }
        }
    }

    /// SIA2 cone search over every configured collection. Collections
    /// are queried concurrently (SIA took 20-33 s per collection near
    /// Barnard's Star on 2026-11-14); the shared semaphore still caps
    /// upstream concurrency.
    async fn sia_search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        radius_deg: f64,
        key: &CacheKey,
    ) -> Result<Vec<Observation>, IrsaError> {
        let sia = format!("{}/SIA", self.inner.cfg.base_url.trim_end_matches('/'));
        let pos = pos_parameter(ra_deg, dec_deg, radius_deg);
        let mut set = JoinSet::new();
        for (idx, collection) in self.inner.cfg.collections.iter().enumerate() {
            let (this, collection, sia, pos, skey) = (
                self.clone(),
                collection.clone(),
                sia.clone(),
                pos.clone(),
                key.clone(),
            );
            set.spawn(async move {
                let body = this.sia_body(&sia, &collection, &pos, &skey).await?;
                let text = String::from_utf8(body)
                    .map_err(|_| IrsaError::Parse("SIA response is not UTF-8".into()))?;
                Ok::<_, IrsaError>((idx, observations_from_votable(&text, &collection)?))
            });
        }
        let mut parts: Vec<(usize, Vec<Observation>)> = Vec::new();
        let mut first_err: Option<IrsaError> = None;
        while let Some(joined) = set.join_next().await {
            let result = joined
                .map_err(|e| {
                    IrsaError::Internal(format!("SIA task failed to join: {}", join_error_kind(&e)))
                })
                .and_then(|r| r);
            match result {
                Ok(part) => parts.push(part),
                Err(e) => {
                    // One collection being down must not hide the others:
                    // keep going and only fail when none answered.
                    warn!("SIA collection failed: {}", e.log_safe());
                    first_err.get_or_insert(e);
                }
            }
        }
        if parts.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        // Keep the configured collection order (dedup: first wins).
        parts.sort_by_key(|(idx, _)| *idx);
        Ok(parts.into_iter().flat_map(|(_, o)| o).collect())
    }

    /// TAP sync cone query (the SIA fallback): one ADQL request
    /// covering every release, mapped onto [`Observation`]s and
    /// disk-cached like SIA bodies (see `tap.rs` for the mapping and
    /// the semantic differences vs SIA).
    async fn tap_search(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        key: &CacheKey,
    ) -> Result<Vec<Observation>, IrsaError> {
        let url = format!("{}/sync", self.inner.cfg.tap_url.trim_end_matches('/'));
        let params = tap::sync_params(ra_deg, dec_deg);
        let query: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let body = self
            .disk_cached_get(key, "tap", &url, &query, Upstream::Tap)
            .await?;
        let text = String::from_utf8(body)
            .map_err(|_| IrsaError::Parse("TAP response is not UTF-8".into()))?;
        tap::observations_from_tap(&text, &self.inner.cfg.base_url, &self.inner.cfg.collections)
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
            self.index_cutout(obs, ra_deg, dec_deg, fov_deg, key);
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
                            Ok(frame) => {
                                // Self-heal: a cutout file written before
                                // sidecars existed gets its `.meta` now so
                                // the coverage index can find it later.
                                disk_write_meta(d, &disk_key).await;
                                return Ok(Entry::Frame(Arc::new(frame)));
                            }
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
                        Ok(small) => {
                            disk_write(d, name, &small).await;
                            disk_write_meta(d, &disk_key).await;
                        }
                        Err(e) => warn!("disk cache: cannot strip cutout: {}", fits_error_kind(&e)),
                    }
                }
                Ok::<_, IrsaError>(Entry::Frame(Arc::new(frame)))
            })
            .await;
        match fetched {
            Ok(Entry::Frame(frame)) => {
                self.index_cutout(obs, ra_deg, dec_deg, fov_deg, key);
                Ok(frame)
            }
            Ok(Entry::Observations(_)) => {
                Err(IrsaError::Internal("cache entry kind mismatch".into()))
            }
            Err(e) => Err(self.remember_failure(key, &e).await),
        }
    }

    /// Lock the coverage index; a poisoned mutex still yields its data
    /// (the index is a hint structure — a panic elsewhere must not
    /// break requests that could proceed without it).
    fn lock_index(&self) -> std::sync::MutexGuard<'_, CoverageIndex> {
        self.inner
            .index
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Record a cached cutout's coverage in the secondary index.
    fn index_cutout(
        &self,
        obs: &Observation,
        ra_deg: f64,
        dec_deg: f64,
        fov_deg: f64,
        key: CacheKey,
    ) {
        self.lock_index().insert_cutout(
            obs.id_hash(),
            CutoutIndexEntry {
                ra: ra_deg,
                dec: dec_deg,
                size: fov_deg,
                key,
            },
        );
    }

    /// Record a cached SIA cone in the secondary index.
    fn index_search(&self, ra_deg: f64, dec_deg: f64, radius: f64, key: CacheKey) {
        self.lock_index().insert_search(SearchIndexEntry {
            ra: ra_deg,
            dec: dec_deg,
            radius,
            key,
        });
    }

    /// Populate the coverage index from the `CACHE_DIR` `.meta`
    /// sidecars, once per process. Best-effort: an unreadable directory
    /// or malformed file only means that entry is not discoverable, so
    /// the request falls back to the normal paths. Entries written
    /// after the scan are indexed at insert time, so no rescan is ever
    /// needed.
    async fn scan_disk_index(&self) {
        let Some(dir) = self.inner.cfg.cache_dir.clone() else {
            return;
        };
        if self.lock_index().disk_scanned {
            return;
        }
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => {
                // Do not retry: a missing/unreadable cache dir means
                // "no disk cache"; the memory paths still work.
                self.lock_index().disk_scanned = true;
                return;
            }
        };
        let mut metas = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.ends_with(".meta") {
                continue;
            }
            if let Ok(text) = tokio::fs::read_to_string(e.path()).await {
                metas.push(text);
            }
        }
        let mut idx = self.lock_index();
        for text in &metas {
            match parse_meta(text, &self.inner.collections_key) {
                Some(Meta::Cutout { ra, dec, size, obs }) => idx.insert_cutout(
                    obs,
                    CutoutIndexEntry {
                        ra,
                        dec,
                        size,
                        key: CacheKey {
                            collections: self.inner.collections_key.clone(),
                            kind: KeyKind::Frame {
                                ra: ra.to_bits(),
                                dec: dec.to_bits(),
                                size: size.to_bits(),
                                obs,
                            },
                        },
                    },
                ),
                Some(Meta::Search { ra, dec, radius }) => idx.insert_search(SearchIndexEntry {
                    ra,
                    dec,
                    radius,
                    key: CacheKey {
                        collections: self.inner.collections_key.clone(),
                        kind: KeyKind::Search {
                            ra: ra.to_bits(),
                            dec: dec.to_bits(),
                            radius: radius.to_bits(),
                        },
                    },
                }),
                None => {}
            }
        }
        idx.disk_scanned = true;
    }

    /// Read-only coverage probe (b18): the smallest cached cutout of
    /// `obs` whose box fully contains `(ra_deg, dec_deg)` plus an
    /// aperture margin of `margin_deg` degrees on the sky. Looks in the
    /// in-memory decoded-frame cache first, then — when `CACHE_DIR` is
    /// set — in the disk cache (the same decoded-from-bytes path the
    /// normal fetch uses). Never performs upstream traffic; `None`
    /// means "no covering cutout cached" and callers fall back to
    /// [`Self::fetch_cutout`].
    ///
    /// `margin_deg` is converted to pixels with each candidate frame's
    /// own CD scale, so callers should pass `needed_px * scale_bound`
    /// where `scale_bound` is at least the frame's native pixel scale
    /// (an over-estimate only makes the check slightly stricter).
    pub async fn cached_cutout_covering(
        &self,
        obs: &Observation,
        ra_deg: f64,
        dec_deg: f64,
        margin_deg: f64,
    ) -> Option<Arc<Frame>> {
        self.scan_disk_index().await;
        // Snapshot the geometrically plausible candidates, smallest
        // first: any point inside a box is within its half-diagonal of
        // the centre, so the filter never drops a true covering cutout.
        let mut candidates: Vec<CutoutIndexEntry> = {
            let idx = self.lock_index();
            idx.cutouts
                .get(&obs.id_hash())
                .map(|es| {
                    es.iter()
                        .filter(|e| {
                            sky_dist_deg(ra_deg, dec_deg, e.ra, e.dec) <= search_radius_deg(e.size)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        candidates.sort_by(|a, b| a.size.total_cmp(&b.size));
        for e in candidates {
            if let Some(Entry::Frame(frame)) = self.inner.cache.get(&e.key).await {
                if frame_covers(&frame, ra_deg, dec_deg, margin_deg) {
                    return Some(frame);
                }
                continue;
            }
            let Some(dir) = &self.inner.cfg.cache_dir else {
                continue;
            };
            let Some(bytes) = disk_read(&dir.join(e.key.file_name("")), None).await else {
                continue;
            };
            match parse_frame(&bytes) {
                Ok(f) => {
                    let frame = Arc::new(f);
                    self.inner
                        .cache
                        .insert(e.key, Entry::Frame(frame.clone()))
                        .await;
                    if frame_covers(&frame, ra_deg, dec_deg, margin_deg) {
                        return Some(frame);
                    }
                }
                Err(err) => warn!(
                    "disk cache: discarding unreadable cutout: {}",
                    fits_error_kind(&err)
                ),
            }
        }
        None
    }

    /// Read-only coverage probe for the SIA result list (b18): an
    /// already-cached cone search (memory or `CACHE_DIR`) whose cone
    /// contains `(ra_deg, dec_deg)`.
    ///
    /// Reuse rule: SIA returns every observation whose footprint
    /// intersects the cone, so a cached result for a cone containing
    /// the click is a superset of "observations covering the click" —
    /// any observation covering the click intersects the cone at the
    /// click itself. The smallest covering cone wins: it carries the
    /// least excess of observations that do not cover the click. Fresh
    /// memory/disk entries perform zero upstream traffic; a stale disk
    /// body is refetched through the disk-cached search path (bounded
    /// by the normal retry policy, serving stale on a transient
    /// failure — see [`Self::disk_cached_get`]). `None` lets the caller
    /// run the real search (at its own, possibly cached, position).
    pub async fn cached_observations_covering(
        &self,
        ra_deg: f64,
        dec_deg: f64,
    ) -> Option<Arc<Vec<Observation>>> {
        self.scan_disk_index().await;
        let mut candidates: Vec<SearchIndexEntry> = {
            let idx = self.lock_index();
            idx.searches
                .iter()
                .filter(|e| sky_dist_deg(ra_deg, dec_deg, e.ra, e.dec) <= e.radius)
                .cloned()
                .collect()
        };
        candidates.sort_by(|a, b| a.radius.total_cmp(&b.radius));
        for e in candidates {
            if let Some(Entry::Observations(obs)) = self.inner.cache.get(&e.key).await {
                return Some(obs);
            }
            let Some(dir) = &self.inner.cfg.cache_dir else {
                continue;
            };
            if let Some(obs) = self.disk_observations(&e.key, dir).await {
                self.inner
                    .cache
                    .insert(e.key, Entry::Observations(obs.clone()))
                    .await;
                return Some(obs);
            }
        }
        None
    }

    /// Rebuild a deduplicated observation list from the per-collection
    /// SIA bodies under `dir`; `None` when any collection body is
    /// missing and could not be refetched, or is unparseable — the
    /// caller then takes the normal search path (never a partial
    /// upstream substitute).
    ///
    /// A body older than the disk TTL is refetched through
    /// [`Self::sia_body`] (fresh data wins when upstream answers; a
    /// transient failure serves the stale body — see
    /// [`Self::disk_cached_get`]), which is what lets a baked cone keep
    /// answering while upstream is unreachable.
    async fn disk_observations(
        &self,
        key: &CacheKey,
        dir: &std::path::Path,
    ) -> Option<Arc<Vec<Observation>>> {
        let mut all = Vec::new();
        for collection in &self.inner.cfg.collections {
            let bytes = match disk_read(
                &dir.join(key.file_name(collection)),
                Some(self.inner.cfg.search_disk_ttl),
            )
            .await
            {
                Some(b) => b,
                None => self.refresh_search_body(key, collection).await?,
            };
            let text = String::from_utf8(bytes).ok()?;
            all.extend(observations_from_votable(&text, collection).ok()?);
        }
        let obs = dedup_by_url(all);
        (!obs.is_empty()).then(|| Arc::new(obs))
    }

    /// Refetch one collection's SIA body for a `Search` key through the
    /// disk-cached path: fresh upstream data when it answers, the stale
    /// disk body on a transient failure. `None` for non-search keys and
    /// on errors that must not be masked (the caller falls back to the
    /// normal search path, which reports the same failure).
    async fn refresh_search_body(&self, key: &CacheKey, collection: &str) -> Option<Vec<u8>> {
        let (ra_deg, dec_deg, radius_deg) = match &key.kind {
            KeyKind::Search { ra, dec, radius } => (
                f64::from_bits(*ra),
                f64::from_bits(*dec),
                f64::from_bits(*radius),
            ),
            KeyKind::Frame { .. } => return None,
        };
        let sia = format!("{}/SIA", self.inner.cfg.base_url.trim_end_matches('/'));
        let pos = pos_parameter(ra_deg, dec_deg, radius_deg);
        self.sia_body(&sia, collection, &pos, key).await.ok()
    }

    /// Diagnostic: entries in the coverage index (for tests; the bound
    /// is documented on [`CoverageIndex`]).
    #[doc(hidden)]
    pub fn coverage_index_len(&self) -> usize {
        self.lock_index().len()
    }

    /// Raw SIA VOTable for one collection, from `CACHE_DIR` when fresh.
    async fn sia_body(
        &self,
        sia: &str,
        collection: &str,
        pos: &str,
        key: &CacheKey,
    ) -> Result<Vec<u8>, IrsaError> {
        self.disk_cached_get(
            key,
            collection,
            sia,
            &[("COLLECTION", collection), ("POS", pos)],
            Upstream::Sia,
        )
        .await
    }

    /// GET `url` with retry (channel `chan` for health accounting),
    /// serving from `CACHE_DIR` when a fresh copy exists and writing the
    /// fetched body back. `suffix` makes the disk file name unique
    /// within one search (collection name for SIA, `"tap"` for TAP).
    async fn disk_cached_get(
        &self,
        key: &CacheKey,
        suffix: &str,
        url: &str,
        query: &[(&str, &str)],
        chan: Upstream,
    ) -> Result<Vec<u8>, IrsaError> {
        let disk = self
            .inner
            .cfg
            .cache_dir
            .as_ref()
            .map(|d| (d.clone(), key.file_name(suffix)));
        if let Some((d, name)) = &disk {
            if let Some(b) = disk_read(&d.join(name), Some(self.inner.cfg.search_disk_ttl)).await {
                // Self-heal the coverage sidecar (same reasoning as the
                // cutout path); written once per collection, idempotent.
                disk_write_meta(d, key).await;
                return Ok(b);
            }
        }
        match self.get_with_retry(url, query, chan).await {
            Ok(b) => {
                if let Some((d, name)) = &disk {
                    disk_write(d, name, &b).await;
                    disk_write_meta(d, key).await;
                }
                Ok(b)
            }
            Err(e) => {
                // Serve-stale (b20): when the disk entry exists but is
                // older than the TTL and the refetch failed transiently
                // (5xx, timeouts, transport), serve the stale body —
                // this is what keeps a baked preset cache answering
                // while upstream is unreachable. Permanent failures
                // (4xx, oversize bodies) and a missing disk entry stay
                // authoritative; fresh data still wins whenever
                // upstream answers.
                if !transient_failure(&e) {
                    return Err(e);
                }
                let Some((d, name)) = &disk else {
                    return Err(e);
                };
                match disk_read(&d.join(name), None).await {
                    Some(b) => {
                        warn!(
                            "disk cache: serving stale search body after upstream failure ({})",
                            e.log_safe()
                        );
                        disk_write_meta(d, key).await;
                        Ok(b)
                    }
                    None => Err(e),
                }
            }
        }
    }

    /// Upstream reachability snapshot for `/api/v1/health`, derived
    /// from the client's own request outcomes — never blocking.
    ///
    /// When no upstream contact finished inside
    /// `health_probe_interval`, one detached SIA probe is spawned (at
    /// most one per interval) so an idle server still reports a fresh
    /// verdict; a busy server never pays for probes.
    pub fn upstream_status(&self) -> UpstreamStatus {
        if self
            .inner
            .health
            .probe_due(self.inner.cfg.health_probe_interval)
        {
            let this = self.clone();
            tokio::spawn(async move { this.probe_sia().await });
        }
        self.inner.health.snapshot()
    }

    /// Cheap SIA probe on the first configured collection: a
    /// millidegree cone search whose outcome lands in the same health
    /// tracker as real traffic (failures are not cached — probes are
    /// detached observability, not user requests).
    async fn probe_sia(&self) {
        let sia = format!("{}/SIA", self.inner.cfg.base_url.trim_end_matches('/'));
        let Some(collection) = self.inner.cfg.collections.first() else {
            return;
        };
        let _ = self
            .get_with_retry(
                &sia,
                &[("COLLECTION", collection.as_str()), ("POS", PROBE_POS)],
                Upstream::Sia,
            )
            .await;
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
        self.get_with_retry(ibe_url, &[], Upstream::Ibe).await
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
        chan: Upstream,
    ) -> Result<Vec<u8>, IrsaError> {
        self.get_retry_on(&self.inner.sem, url, query, None, chan)
            .await
    }

    /// GET the last `n` bytes of `url` from the S3 mirror (suffix range).
    async fn get_tail(&self, url: &str, n: usize) -> Result<Vec<u8>, IrsaError> {
        self.get_retry_on(
            &self.inner.s3_sem,
            url,
            &[],
            Some(ByteRange::Suffix(n)),
            Upstream::S3,
        )
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
                Upstream::S3,
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
        chan: Upstream,
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
                Attempt::Ok(body) => {
                    self.inner.health.record(chan, true);
                    return Ok(body);
                }
                Attempt::Fatal(e) => {
                    // A real HTTP answer below 500, or an oversized
                    // body, still proves the service is reachable.
                    self.inner.health.record(chan, contact_ok(&e));
                    return Err(e);
                }
                Attempt::Retryable(detail) => {
                    if attempt >= self.inner.cfg.max_attempts {
                        self.inner.health.record(chan, false);
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

/// SIA probe cone: a millidegree circle at the origin — tiny result,
/// still exercises the real service path.
const PROBE_POS: &str = "circle 0 0 0.0001";

/// Whether an upstream failure is transient: retry exhaustion
/// (503/429/timeout/transport after all retries) or a non-retryable
/// 5xx. Drives both the SIA->TAP fallback and the disk-cache
/// serve-stale path. 4xx answers, parse failures and empty results are
/// authoritative and are returned unchanged.
fn transient_failure(e: &IrsaError) -> bool {
    matches!(e, IrsaError::Unavailable { .. })
        || matches!(e, IrsaError::Http { status } if *status >= 500)
}

/// Whether finishing with `e` still proves the upstream answered:
/// any HTTP status below 500 (4xx included — the service is alive and
/// judged the request) or an oversized body.
fn contact_ok(e: &IrsaError) -> bool {
    match e {
        IrsaError::Http { status } => *status < 500,
        _ => true,
    }
}

/// Write the `.meta` coverage sidecar of `key` into `dir` (same
/// best-effort atomic write as the data files; see
/// [`CacheKey::meta_name`]).
async fn disk_write_meta(dir: &std::path::Path, key: &CacheKey) {
    disk_write(dir, &key.meta_name(), key.meta_body().as_bytes()).await;
}

/// Angular distance in degrees between two sky positions, small-angle
/// approximation (fields here are well under a degree) with RA wrapped
/// into [-180, 180).
fn sky_dist_deg(ra: f64, dec: f64, cra: f64, cdec: f64) -> f64 {
    let dra = (ra - cra + 540.0).rem_euclid(360.0) - 180.0;
    let ddec = dec - cdec;
    let x = dra * f64::midpoint(dec, cdec).to_radians().cos();
    x.hypot(ddec)
}

/// True when `frame`'s pixel box fully contains the sky point plus a
/// margin of `margin_deg` degrees on the sky: the point maps at least
/// `margin_deg / scale` pixels inside every array edge, where `scale`
/// is the frame's own CD pixel scale. That is exactly what the spectrum
/// aperture needs — a click near the cutout edge whose aperture would
/// fall off the array is NOT covered and must not be served from this
/// frame.
fn frame_covers(frame: &Frame, ra_deg: f64, dec_deg: f64, margin_deg: f64) -> bool {
    let cd = frame.wcs.cd;
    let scale = (cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]).abs().sqrt();
    if !(scale.is_finite() && scale > 0.0) {
        return false;
    }
    let Ok((px, py)) = frame.wcs.radec_to_pixel(ra_deg, dec_deg) else {
        return false;
    };
    let (ny, nx) = frame.image.dim();
    let m = margin_deg / scale;
    px.is_finite()
        && py.is_finite()
        && px - m >= 0.5
        && px + m <= nx as f64 + 0.5
        && py - m >= 0.5
        && py + m <= ny as f64 + 0.5
}

/// One parsed `.meta` sidecar line (see [`CacheKey::meta_body`]).
enum Meta {
    /// Cutout geometry: centre, square side, observation id hash.
    Cutout {
        /// Cutout centre RA, degrees.
        ra: f64,
        /// Cutout centre Dec, degrees.
        dec: f64,
        /// Requested square side, degrees.
        size: f64,
        /// Observation id hash (FNV-1a of the access URL).
        obs: u64,
    },
    /// SIA cone: centre and radius.
    Search {
        /// Cone centre RA, degrees.
        ra: f64,
        /// Cone centre Dec, degrees.
        dec: f64,
        /// Cone radius, degrees.
        radius: f64,
    },
}

/// Parse a `.meta` sidecar body for a client whose collection set is
/// `collections`; `None` on any format drift or set mismatch — foreign
/// or corrupt files are skipped, never fatal.
fn parse_meta(text: &str, collections: &str) -> Option<Meta> {
    let mut t = text.split_whitespace();
    if t.next() != Some("v1") {
        return None;
    }
    let kind = t.next()?;
    if t.next()? != collections {
        return None;
    }
    let bits = |tok: Option<&str>| {
        tok.and_then(|s| u64::from_str_radix(s, 16).ok())
            .map(f64::from_bits)
    };
    match kind {
        "search" => Some(Meta::Search {
            ra: bits(t.next())?,
            dec: bits(t.next())?,
            radius: bits(t.next())?,
        }),
        "cutout" => Some(Meta::Cutout {
            ra: bits(t.next())?,
            dec: bits(t.next())?,
            size: bits(t.next())?,
            obs: u64::from_str_radix(t.next()?, 16).ok()?,
        }),
        _ => None,
    }
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
