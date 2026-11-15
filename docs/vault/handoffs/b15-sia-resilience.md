# Handoff: b15-sia-resilience

Status: **done**. Branch `devin/b15-sia-resilience`. Backend only; the
contract file was NOT edited (see "Contract note" — the `Health` schema
permits the optional fields added, and the orchestrator may want to
write them into it explicitly).

## What was built

1. **TAP fallback for the SIA search** (`backend/src/irsa/tap.rs`,
   `client.rs`, `config.rs`). When every SIA attempt fails transiently
   — `IrsaError::Unavailable` (503/429/timeout/transport after all
   retries) or `IrsaError::Http { status >= 500 }` (e.g. the 502s seen
   during testing) — `search` issues one ADQL cone query against the
   IRSA TAP service and maps the rows onto the same `Observation`
   type. The result flows through the same `try_get_with` cache entry,
   so fallback results are cached and coalesced exactly like SIA
   results (and disk-cached under suffix `"tap"` when `CACHE_DIR` is
   set). 4xx, parse failures and `NoObservations` stay authoritative —
   they never trigger the fallback. When both paths fail, the original
   SIA error is the one propagated (502 at the API via the existing
   `Http{5xx}/Unavailable -> UpstreamUnavailable` mapping).

   Config (all in `IrsaConfig`, env-parsed, documented in the module
   table):
   - `SIA_FALLBACK_TAP` (default on): master switch.
   - `IRSA_TAP_URL` (default `https://irsa.ipac.caltech.edu/TAP`):
     endpoint base; `/sync` is appended.
   - `HEALTH_PROBE_SECS` (default 60 at merge; raised to **300** in b19
     per orchestrator decision): min spacing between probes.

2. **Upstream health on `/api/v1/health`** (`api/health.rs`,
   `client.rs`, `api/mod.rs`). Every upstream exchange is recorded in
   the client's `Health` tracker at the point it finishes
   (`get_retry_on`): a 2xx body, any HTTP answer < 500 (4xx included —
   the service is alive and judging requests) or an oversized body
   count as *reachable*; 5xx and retry exhaustion count as *down*.
   `/health` now returns:

   ```json
   { "status":"ok", "version":"...", "collections":[...],
     "upstream_ok": true,
     "upstream": { "last_sia_ok": 1763400000, "last_tap_ok": null,
                   "last_ibe_ok": 1763400100, "last_s3_ok": 1763400050 } }
   ```

   `upstream_ok` = outcome of the most recently finished upstream
   contact (optimistic `true` before any traffic). `last_*_ok` = Unix
   seconds of the last successful contact per channel; `null` when the
   channel never succeeded. `FrameSource` gained a defaulted
   `upstream_health()` so existing test stubs are untouched.

   Probe: `/health` never blocks. When no upstream contact finished
   inside `health_probe_interval` AND no probe ran in that window, one
   detached SIA cone (`POS=circle 0 0 0.0001`, first collection) is
   spawned — at most one probe per interval, zero extra traffic while
   the app serves real requests (the probe's outcome is recorded like
   any other). Probes do NOT write to caches; they are observability
   only.

## Verified TAP facts (live, 2026-11-14)

- Endpoint: `https://irsa.ipac.caltech.edu/TAP` — `GET
  /TAP/availability` -> `<available>true</available>`; queries go to
  `POST|GET /TAP/sync` with `REQUEST=doQuery&LANG=ADQL(&FORMAT=votable)`.
- Doc URL (pattern source): the IRSA "Spectral Image Cutouts"
  notebook, <https://caltech-ipac.github.io/irsa-tutorials/spherex-cutouts/>
  (also `spherex-psf` and `spherex-source-discovery-tool-demo` use the
  same JOIN/`CONTAINS(POINT, p.poly)` shape).
- Tables: `spherex.artifact a` (`planeid`, `uri`, `producttype`,
  `releasetype`, ...) JOIN `spherex.plane p` (`planeid`, `poly`,
  `time_bounds_lower/upper` unit d, `energy_bounds_lower/upper` unit
  m, `energy_bandpassname`, ...) ON `a.planeid = p.planeid`.
- Real query at the WISE 0855 epoch-2026.0 position (RA 133.81891,
  Dec -7.24705, the preset in `api/targets.rs`):

  ```sql
  SELECT a.uri, a.producttype, a.releasetype, p.time_bounds_lower,
         p.time_bounds_upper, p.energy_bounds_lower,
         p.energy_bounds_upper, p.energy_bandpassname
  FROM spherex.artifact a
  JOIN spherex.plane p ON a.planeid = p.planeid
  WHERE 1=CONTAINS(POINT('ICRS',133.81891,-7.24705), p.poly)
  ORDER BY p.time_bounds_lower
  ```

  -> **261 rows**, QUERY_STATUS OK. All `producttype='science'`,
  `releasetype='data'`, every `uri` of the form
  `ibe/data/spherex/<release>/level2/.../level2_..._spx_....fits`
  (`dr1` only at this position). `time_bounds_*` are MJD days,
  `energy_bounds_*` are metres — same units SIA emits.
- **Not supported**: `1=INTERSECTS(CIRCLE('ICRS',...), p.poly)`
  returns 200 + `QUERY_STATUS=ERROR` "Invalid or unsupported ADQL
  query string" — so footprint-vs-FOV-square overlap cannot be
  expressed; the fallback tests the query *point* only (see
  Deviations). `p.datarelease`/`metarelease` are timestamps, not
  release names — the release is recovered from the artifact path.
- Latency note: TAP answered in ~2 s for TAP_SCHEMA but ~2 min for
  `spherex.plane` queries today (the notebook cites ~37 s). It is a
  fallback, and the API's detached-search design keeps filling the
  cache after the 40 s deadline, but under an SIA outage the first
  caller may still hit the request deadline before TAP answers.

## Decisions / deviations

- **Semantics kept**: `access_url = IRSA_BASE_URL + "/" + a.uri`
  reproduces the SIA access URL (verified shape above); dedup by
  `access_url`, collection ordering and `select_frames` ordering are
  unchanged — downstream code cannot tell which path produced the
  rows. Ordering inside the TAP list is `time_bounds_lower` ascending
  (notebook order); `select_frames` re-sorts by epoch anyway.
- **Deviation (documented, unavoidable)**: TAP tests only the center
  point, not the whole FOV square. An exposure covering the square's
  edge but missing its centre is missed — the same rows the IBE
  cutout service would later 422 on anyway. No radius is sent.
- **Deviation**: SIA queries each configured collection separately;
  TAP returns all releases in one query and the release is recovered
  from the `ibe/data/spherex/<rel>/` path segment
  (`spherex_<rel>`; `_` also matches a `/` boundary so
  `spherex_dr1_deep` accepts `dr1_deep/` or `dr1/deep/`). Rows whose
  release is not configured are dropped. Only
  `a.producttype='science'` artifacts are queried (verified: all
  artifacts at the test position are `science`).
- **Degradation**: TAP VOTables lacking `uri`, the bounds columns or
  `energy_bandpassname` still parse — missing numerics become NaN and
  rows without a usable `uri`/release match are skipped, exactly like
  SIA rows without `access_url`. A `QUERY_STATUS=ERROR` VOTable maps
  to `IrsaError::Parse`.
- **Contract note**: `Health` in `shared/openapi-contract.json` has no
  `additionalProperties`, so the extra `upstream_ok`/`upstream` fields
  are schema-legal and no `/api/v1/health/upstream` endpoint was
  created. The orchestrator may still want to declare the optional
  fields in the contract for frontend typing.
- **`SI_USAGE.md` is outside this task's scope** — not edited. Please
  record there that Devin wrote tap.rs, the health fields, client
  fallback/probe changes and the new test file.
- The vault's open question "TAP endpoint URL" is now resolved
  (above); `open-questions.md` was not edited (out of scope).

## Files

- `backend/src/irsa/tap.rs` (new): ADQL builder, `/sync` params, TAP
  VOTable -> `Observation` mapping, release/collection matching.
- `backend/src/irsa/client.rs`: `search` refactored into
  `sia_search`/`tap_search` under `search_observations`; `Health`
  tracker + `UpstreamStatus`; `get_retry_on` records outcomes per
  channel (`Upstream::{Sia,Tap,Ibe,S3}`); `upstream_status()` +
  detached `probe_sia`; shared `disk_cached_get` used by both SIA and
  TAP bodies.
- `backend/src/irsa/config.rs`: `sia_fallback_tap`, `tap_url`,
  `health_probe_interval` + env parsing + doc table.
- `backend/src/irsa/mod.rs`: `mod tap`, exports (`UpstreamStatus`,
  `DEFAULT_TAP_URL`, `tap_cone_query`, `observations_from_tap`).
- `backend/src/api/health.rs` (new): handler + extended response DTO.
- `backend/src/api/mod.rs`: route moved to `health::get`,
  `FrameSource::upstream_health` (defaulted), `IrsaClient` impl.
- `backend/tests/sia_resilience_tests.rs` (new): 10 tests.

## Verify

```sh
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Result (2026-11-14): fmt clean, clippy clean, **156 tests pass**
(10 new in `sia_resilience_tests` covering: TAP row mapping/units,
QUERY_STATUS=ERROR, SIA-502->TAP fallback end-to-end with dedup and
cache hit, both-down->502 through the router, fallback disabled->no
TAP traffic, 4xx->no fallback, health upstream_ok transitions
false->true with timestamps, probe rate limit). Two live tests in
`irsa_tests` ran against real IRSA and passed.

## Open issues / follow-ups

- TAP latency (~2 min observed) can exceed `REQUEST_DEADLINE_SECS` —
  results still land in cache for the retry, but consider raising
  TAP-side patience or documenting degraded UX during SIA outages.
- The probe exercises SIA only; TAP/S3 health is inferred from real
  traffic (deliberate — keeps probes to <=1/interval and cheap).
- `spherex_*_deep` release-path layout is assumed (`<rel>/` or
  `<rel with _ -> />/`); unverified against real deep artifacts.
- `/health` additions should be declared in the contract if the
  frontend is to consume them (orchestrator decision).
