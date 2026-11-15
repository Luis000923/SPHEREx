# Handoff: b14-spectrum-latency

Status: **done** (backend only). Branch `devin/b14-spectrum-latency`,
worktree `.devin-worktrees/b14-spectrum-latency`. Not pushed.

## Files

- `backend/src/api/spectrum.rs` (modified): upstream-position
  quantisation + documentation. No behavioural change to the
  deadline/error pipeline itself — see "What was already true".
- `backend/tests/spectrum_latency_tests.rs` (new): 8 tests, latency
  contract of `/spectrum` against a stub `FrameSource`. No network.

## What was already true (important)

`/spectrum` already shares the `/frames` fetch machinery since
`integration-live` (`api/fetch.rs`): one request deadline covering the
SIA search, a soft deadline that answers once >= 2 cutouts decoded,
detached background completion of the abandoned fetches (they keep
filling the client cache), negative caching of failures, and "error
only when zero points exist". The measured "cold request: 28.9 s,
2 points" **was the soft deadline working**: the SIA search consumed
most of the 40 s budget near Barnard's Star and only 2 cutouts had
decoded when the second point arrived. Nothing in `fetch.rs` needed
changing, and the >= 2 gate was kept deliberately ("the same
behaviour" as `/frames`; a 1-point spectrum answer is not obviously
better and the task's own test spec asks for >= 2).

## What was actually broken and is now fixed

Every spectrum click carries the **clicked pixel's** coordinates, whose
f64 bits are a different upstream position per click. The client cache
keys are `(collections, ra bits, dec bits, size bits, obs hash)` for
cutouts and `(collections, ra, dec, radius)` for searches
(`irsa/client.rs`), so **every click paid a fresh SIA search (~20-33 s
at Barnard) plus a full set of cutout downloads**, even arcseconds away
from a warm position.

`spectrum.rs` now snaps the *upstream* position (SIA cone centre and
cutout centre) to a `SPECTRUM_POS_CELL_DEG = 0.005` (~18") grid before
calling `search_until`/`fetch_cutouts`:

- Clicks inside one cell share the search entry, the decoded cutouts
  and `try_get_with` coalescing — a second click in a warm cell is a
  cache hit end to end.
- The cell stays well under the 0.01 deg cutout half-side (18"), so a
  click is at most cell/sqrt(2) ~ 12.7" off-centre and always inside
  the cutout; the search cone (half the cutout diagonal) still covers
  the whole cell. The aperture extraction and the JSON response keep
  the exact requested coordinates.
- Edge cases: `ra` wraps via `rem_euclid` (a click at 359.99x snapping
  to 360.0 becomes 0.0), `dec` is clamped to [-90, 90] (float noise at
  the poles could otherwise snap just past the validated bound and fail
  `check_query` upstream).

## Reuse of `/frames` cutouts: NOT possible within scope

As suspected in the task text: a `/frames` cutout is cached under the
exact `(ra, dec, fov)` of the frames request, and the spectrum handler
only knows the clicked `(ra, dec)` — the two never bit-match, so no key
the handler can construct hits them. `FrameSource` (`api/mod.rs`, out
of scope) exposes no cache probe either. The minimal hook needed:

```rust
// irsa/client.rs — read-only probe over the decoded-frame cache.
// Either iterate `self.inner.cache.iter()` and keep `KeyKind::Frame`
// entries with `obs == obs.id_hash()` whose (centre, size) box covers
// (ra_deg, dec_deg) plus an aperture margin, or maintain a secondary
// index obs_hash -> [(ra, dec, size)] updated on insert.
pub async fn cached_cutout_covering(
    &self, obs: &Observation, ra_deg: f64, dec_deg: f64,
) -> Option<Arc<Frame>>

// api/mod.rs — extend the trait (default impl returns None):
fn cached_cutout(
    &self, obs: &Observation, ra_deg: f64, dec_deg: f64,
) -> impl Future<Output = Option<Arc<Frame>>> + Send + '_;
```

`spectrum.rs` would then probe each selected observation before
queueing its 0.01 deg download (a cached 0.05/0.1 deg `/frames` cutout
is a valid superset for the <= 5 px aperture whenever it covers the
click). Until that exists, spectrum clicks can only reuse
spectrum-fetched (and now cell-shared) cutouts.

## "Partial" indicator: not added (contract note)

`SpectrumResponse` has only `ra_deg`, `dec_deg`, `unit`, `points` — a
partial-result flag cannot be expressed without a contract change, so
none was added; `points.len()` is the final count of that response.
Same gap `/frames` solved with `truncated` + `total_observations`. If a
partial indicator is wanted, propose `points_final`/`total_selected`
on `SpectrumResponse` to the orchestrator. Meanwhile the frontend
spectrum hook does NOT poll (unlike `useFrames`), so a cold first click
shows whatever decoded by the deadline; a later click in the same cell
is instant.

## Latency before/after (stub, no network)

Stub: `LatencyStub`, 12 observations, 3 s per cold cutout, upstream
concurrency 3 (batches decode at ~3/6/9/12 s), soft deadline 4 s,
request deadline 30 s. The stub models the real client's position-
keyed cache, so the "before" column is what a non-quantised click pays.

| Scenario | Before | After |
|---|---|---|
| Cold click | 200 at ~4 s, 3/12 pts, rest in background | identical (unchanged machinery) |
| Identical request after drain | 200 <100 ms, 12/12 pts | identical |
| Click +10" away | **cold again**: new search + 12 cold fetches (~4 s partial, ~12 s full) | **all cache hits: <300 ms, 12/12 pts, 0 extra upstream calls** |
| Click in the next cell | cold | cold (unchanged) |

Measured in `spectrum_latency_tests.rs`: `upstream_calls` stays 4 after
the +10" click and `fetch_positions`/`search_positions` show identical
quantised tuples; the background counter returns to 0 with each
observation fetched exactly once.

## Live verification: not run

A live cold request against Barnard's Star (epoch-2026 preset
`ra=17h57m47.102s` -> 269.4463 deg, `dec=+04d46m05.97s` -> 4.7683 deg)
was prepared (local server on :8899, memory-only cache) but not
executed — the live call was declined in-session, so per the task rule
("if IRSA/S3 answers") this section reports only stub numbers. IRSA and
the S3 mirror were reachable at the time (index pages answered, slowly).

## Test coverage added (`spectrum_latency_tests.rs`, 8 tests)

- `cold_request_answers_at_soft_deadline_with_partial_points`: 200 at
  the soft deadline, >= 2 points, sorted, contract key set.
- `background_fetches_drain_to_zero`: in-flight counter returns to 0,
  each observation fetched exactly once (no unbounded tasks).
- `second_identical_request_is_complete_and_fast`: 12/12 points fast.
- `nearby_clicks_share_the_upstream_cache`: same cell = 0 new upstream
  calls + identical quantised tuples; next cell = refetch.
- `failed_cutouts_do_not_error_while_others_succeed`: 2x 422 among 6 ->
  200 with 4 points.
- `all_cutouts_failing_returns_the_contract_error`: all-422 -> 500
  `internal` (existing `IrsaError::Http(4xx) -> internal` mapping from
  b5, contract-valid; arguably a 404 would be nicer — untouched).
- `nothing_decoded_by_the_deadline_is_502_and_fetches_continue`.
- `slow_search_counts_against_the_deadline`: 502 while the detached
  search keeps running.

## Deviations / notes

- The >= 2 soft-deadline gate is shared with `/frames` by design (the
  task asked for "the same behaviour"); answering at >= 1 would need a
  spectrum-local fetch loop duplicating `fetch.rs`.
- `SI_USAGE.md` not updated: outside the allowed scope; the existing
  generic Devin row already covers this task's commits.
- All-422 -> 500 `internal` is pre-existing b5 semantics, left as is.

## Verify

```
cd backend
cargo fmt --check                                   # clean
cargo clippy --all-targets -- -D warnings           # clean
cargo test                                          # 159 passed, 0 failed
cargo test --test spectrum_latency_tests            # 8 passed (~12 s)
```

Full suite: api 25, fits 14, fuzz 12, irsa 25, perf 7, science 15,
science_validation 12, security 28, spectrum_latency 8, targets 5,
validation 8 — all green.
