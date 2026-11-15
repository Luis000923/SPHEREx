# Handoff: b18-spectrum-cache-reuse

Status: **done** (backend only). Branch `devin/b18-spectrum-cache-reuse`,
worktree `.devin-worktrees/b18-spectrum-cache-reuse`. Not pushed.

`/spectrum` now reuses what `/frames` already cached — in memory and
in `CACHE_DIR` — so a click inside a downloaded (or baked) field costs
zero upstream traffic and answers in well under a second. This is the
offline story for the baked presets the task asked for.

## Files

- `backend/src/irsa/client.rs` (modified): coverage index over the
  decoded-result cache, `.meta` sidecars for disk entries, disk index
  scan, and the two read-only probes
  `cached_cutout_covering` / `cached_observations_covering`.
- `backend/src/api/mod.rs` (modified): `FrameSource` gains
  `cached_cutout` and `cached_observations` with default `None`
  implementations (existing stubs still compile); `IrsaClient`
  delegates to the new probes.
- `backend/src/api/spectrum.rs` (modified): probes the cache before
  searching/fetching; only observations with no covering cutout fall
  back to the b14 cell-snapped download path.
- `backend/tests/spectrum_cache_reuse_tests.rs` (new): 6 tests (a–f
  from the task), no network.

## What was built

### Coverage index (`client.rs`)

A `CoverageIndex` (std `Mutex`, never held across `.await`, poisoned
lock still yields data) is filled every time an entry enters or hits
the decoded-result cache:

- `cutouts`: observation id hash -> FIFO `VecDeque` of
  `(ra, dec, size, CacheKey)` geometries,
- `searches`: FIFO `VecDeque` of `(ra, dec, radius, CacheKey)` cones.

Lookups are O(entries of that observation / remembered cones), never a
full cache scan — this is what lets a click find "any cutout of this
observation covering this point" when the click's own coordinates are
not a cache key (the problem b14 documented as unsolvable without this
hook).

**Bounds** (all caps FIFO-drop-oldest): `INDEX_MAX_OBS = 1024` obs
buckets x `INDEX_MAX_PER_OBS = 64` geometries + `INDEX_MAX_SEARCHES =
512` cones <= 65 000 small entries (a few MiB worst case). Index
entries are **not** removed when moka evicts the value they point to —
a stale entry just degrades to a `cache.get` miss (plus one disk probe
when `CACHE_DIR` is set) before falling back to the normal fetch. This
is documented on the struct: the index is a bounded hint structure,
never a source of truth. `coverage_index_len()` (doc-hidden) is the
test hook for the bound test.

### Disk persistence

Each `CACHE_DIR` data file gets a `*.meta` sidecar
(`v1 search|cutout <collections> <geometry as f64 bits hex>`), written
atomically alongside the data file. A fresh client scans the
`*.meta` files once per process (`scan_disk_index`, lazy on first
probe) and rebuilds the coverage index without reversing the hashed
data-file names. Entries written before sidecars existed self-heal:
the first disk read that finds one writes its `.meta`. Foreign
`.meta` files (different collection set, different format version)
are skipped.

### Probes

- `cached_cutout_covering(obs, ra, dec, margin_deg)`: index candidates
  for the observation, geometric prefilter (point within the cutout
  box's half-diagonal — never drops a true covering cutout), smallest
  `size` first, in-memory `cache.get` then disk decode; each candidate
  is validated pixel-exact by `frame_covers`: `radec_to_pixel` plus
  `margin_deg / sqrt(|det CD|)` pixels must stay inside
  `[0.5, n+0.5]` on both axes — a click near the edge whose aperture
  would fall off the array is **not** served from that cutout.
- `cached_observations_covering(ra, dec)`: smallest cached SIA cone
  containing the click; memory first, then disk VOTable files (all
  collections must be present, fresh and parseable — a partial list is
  never substituted for an upstream search).

**SIA-cone reuse rule** (also documented on the method): SIA returns
every observation whose footprint intersects the cone, so a cached
result for a cone containing the click is a *superset* of "observations
covering the click" — any observation covering the click intersects
the cone at the click itself. The 24-observation selection
(`select_frames`, same as `/frames` raw) then runs over that superset.

### Handler (`spectrum.rs`)

1. `cached_observations(click)`; on `None`, `search_until` at the b14
   cell-snapped position (unchanged fallback).
2. `select_frames(observations, min(max_frames_cap, 24))` — identical
   selection to `/frames` raw, so it matches what was cached.
3. Per selected observation `cached_cutout(obs, click, margin)` with
   `margin = (aperture_px/2 + 0.5) px * 9"/px` (conservative native
   scale bound; converted per-frame by `frame_covers`).
4. `fetch_cutouts` over only the missing observations at the
   cell-snapped position; cached + fetched frames go through
   `extract_spectrum` at the exact clicked coordinates. Response shape
   unchanged, points sorted by wavelength.

## Decisions / deviations

- `FrameSource` gained `cached_observations` in addition to the
  task's `cached_cutout`. Without it the "no upstream SIA search when
  a cached cone covers the click" requirement cannot be met — the
  handler needs an observation list before it can select anything.
  Both default to `None`, so existing stubs are unaffected.
- Margin is expressed in degrees (9"/px conservative native scale ->
  up to ~5 px at a 5-px aperture) and converted with each frame's own
  CD scale, per the task's "up to 5 px at the native scale of that
  frame". A slightly over-large degree margin only makes the check
  stricter.
- Index entries are not tracked on moka eviction (see "Bounds" above).
  Chosen over `eviction_listener`/`weigher` changes to keep the patch
  local and keep the index a pure hint.
- `SI_USAGE.md` not updated: outside the allowed scope; the generic
  Devin row already covers this task's commits (same convention as b14).

## What is NOT reused

- A different collection set (`IRSA_COLLECTIONS` change): sidecars and
  cache keys are collections-scoped, so baked files are skipped and
  the request goes upstream — correct, but means presets must be baked
  with the production collection list.
- Clicks outside every cached cone **and** outside all covering
  cutouts: normal download path (search + per-obs cutout).
- Clicks whose aperture would fall off every cached cutout: normal
  download path (this is correctness, not a gap).
- SIA results older than `SEARCH_DISK_TTL` on disk are treated as
  absent.

## Open issues

- The disk-scan is whole-directory; a `CACHE_DIR` shared with other
  tools or holding thousands of stale files costs one directory scan
  per process (bounded by meta file count; skipped once scanned).
- `cached_observations_covering` re-parses VOTable files from disk on
  each cold hit; results are inserted into the moka cache so the cost
  is once per entry.

## Latency

- Before (b14): a click inside a warm field but outside the 0.005-deg
  cell paid a fresh SIA search (~20–33 s measured at Barnard's Star in
  the b14 live runs) plus up to 24 cutout downloads; worst case the
  request deadline.
- After: a click inside a cached field = index lookups + a few
  `cache.get` / disk decodes — no upstream calls at all. On the stub
  the cached `/spectrum` answers in milliseconds (asserted `< 1 s` in
  tests; the whole 6-test file runs in ~0.9 s including the
  restart-scan and concurrency tests).

## Verify

```
cd backend
cargo fmt --check                                   # clean
cargo clippy --all-targets -- -D warnings           # clean
cargo test                                          # 165 passed, 0 failed
cargo test --test spectrum_cache_reuse_tests        # 6 passed (~0.9 s)
```

Full suite: api 25, fits 14, fuzz 12, irsa 25, perf 7, science 15,
science_validation 12, security 28, spectrum_latency 8,
spectrum_cache_reuse 6, targets 5, validation 8 — all green.

New tests (`spectrum_cache_reuse_tests.rs`):

- (a) `spectrum_inside_cached_field_is_fully_served_from_cache` —
  `/frames` warm-up then `/spectrum` ~10" away: zero search and zero
  fetch calls, points bit-for-bit equal to a direct `extract_spectrum`
  over the cached frames, sorted by wavelength.
- (b) `spectrum_outside_cached_field_downloads_as_before` — a 0.6-deg
  click pays the normal search + fetches.
- (c) `edge_click_with_aperture_off_the_cutout_is_not_served_cached` —
  a click inside the box but within the aperture margin of the edge
  triggers downloads, not a clipped cached spectrum.
- (d) `disk_cached_cutouts_serve_spectrum_after_restart` — `CACHE_DIR`
  + a new `IrsaClient`: disk scan finds search cone + cutouts, zero
  upstream calls.
- (e) `coverage_index_stays_bounded_across_requests` — 1000 distinct
  searches/fetches against the real client: index stays <= 704 entries
  (3 obs x 64 + 512 cones).
- (f) `concurrent_identical_spectrum_requests_coalesce` — 8 concurrent
  identical requests: 1 SIA hit + 3 cutout hits upstream
  (`try_get_with` coalescing intact).
