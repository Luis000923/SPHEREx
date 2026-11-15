# Handoff: b20-preset-snap-stale — preset snap + serve-stale

Status: **done**. Rounded / sexagesimal / re-typed preset coordinates now
snap to the preset constants before the cache lookup, aged disk search
bodies are served when a refetch fails transiently, and the search TTL
is `SEARCH_DISK_TTL_SECS`. Verified end to end: release binary against
the 3-day-aged baked cache with dead upstreams, and `docker run
--network none` with the entrypoint mtime refresh bypassed — all 35
preset checks returned 200.

## What was built

- **Snap to preset** (`backend/src/api/params.rs`): after validation,
  `snap_to_preset(ra, dec, fov)` rewrites a `/frames` position to a
  preset's exact constants when it lies within `PRESET_SNAP_ARCSEC`
  (documented const) of that preset on the sky and `fov` is within
  `1e-3` deg of the preset's. Distance uses `angular_dist_deg`, a
  haversine over the sphere — RA 0/360 wrap is absorbed by the sine and
  the RA convergence near the poles falls out of the `cos(dec)`
  factors. `frames_params` applies it before the mode/detector/etc.
  parsing, so upstream keys, the memory/disk cache keys (which hash the
  exact f64 bits) and the `query.*_deg` response echo all see the
  snapped constants. Nothing else changes for non-preset positions or
  `fov` mismatches. `PRESET_SNAP_ARCSEC` and the helper are `pub` so
  tests exercise them directly.
- **Serve-stale** (`backend/src/irsa/client.rs`):
  - `disk_cached_get`: on a stale disk entry the refetch runs as
    before; if it fails with a *transient* error (the renamed
    `transient_failure` = `Unavailable` or non-retryable 5xx — the same
    predicate that drives the SIA→TAP fallback) the stale body is
    re-read with `disk_read(.., None)` and served, with a sanitised
    `warn!` (`e.log_safe()` — error class + host only, no URL paths).
    Fresh data still wins whenever upstream answers; 4xx/oversize
    errors and missing entries keep their old behaviour.
  - `disk_observations` (the b18 spectrum coverage-probe path): a stale
    per-collection body is now refetched through `sia_body` →
    `disk_cached_get` (fresh upstream data, or the stale body on
    transient failure) instead of being abandoned to the normal search
    path. A `refresh_search_body` helper reconstructs the cone from the
    `Search` cache key. Missing-and-failed or unparseable bodies still
    fall back to the normal search path, so no partial upstream
    substitute is ever served.
- **Configurable TTL** (`backend/src/irsa/config.rs`): new
  `search_disk_ttl: Duration` on `IrsaConfig`, env
  `SEARCH_DISK_TTL_SECS` (default 86400), parsed with the other
  `env_u64` knobs. `0` = every disk entry is stale: upstream tried
  first every time, stale data fills in on failure. The hardcoded
  `SEARCH_DISK_TTL` const in `client.rs` is gone.
- **Docs/scripts**: `docs/DEPLOY.md` (env row + "Freshness and
  serve-stale" + "Coordinate snapping" paragraphs),
  `docker-compose.yml` (`SEARCH_DISK_TTL_SECS` env),
  `scripts/container.env.example` (documented var),
  `scripts/docker-entrypoint.sh` (comment rewritten: the mtime refresh
  is kept as belt and braces — serve-stale makes it optional; it still
  avoids a pointless refetch attempt per request and lets new ingests
  land the moment the network is back).

## Files changed

- `backend/src/api/params.rs` — snap consts + `angular_dist_deg` +
  `snap_to_preset` + wiring in `frames_params`.
- `backend/src/api/targets.rs` — unchanged (snap reads
  `preset_targets()` through the crate path; no edit needed).
- `backend/src/config.rs` — unchanged (`IrsaConfig` is embedded; no
  top-level change needed).
- `backend/src/irsa/config.rs` — `search_disk_ttl` field + env parsing.
- `backend/src/irsa/client.rs` — serve-stale in `disk_cached_get`,
  refetch in `disk_observations`, `refresh_search_body`,
  `sia_failed` → `transient_failure`.
- `backend/tests/preset_snap_tests.rs` — new, 9 tests.
- `docs/DEPLOY.md`, `docker-compose.yml`,
  `scripts/container.env.example`, `scripts/docker-entrypoint.sh`.
- This handoff.

## Decisions / deviations

- **`PRESET_SNAP_ARCSEC = 0.3`, not 0.1 arcsec.** The task text says
  0.1" but also requires 4-decimal roundings to snap; the worst-case
  independent ra/dec rounding to 4 decimals is
  `sqrt(2) · 5e-5 deg` ≈ 0.255" (observed max across the five presets
  ≈ 0.19"), which already exceeds 0.1". 0.3" satisfies "4 decimals
  snap" and "0.5" does not" with margin on both sides. Documented on
  the const, in DEPLOY.md and here.
- **Snap applies to `/frames` only**, as specified (`api/params.rs`
  "the single place" for fov-keyed upstream/cache lookups). `/spectrum`
  has no `fov` and benefits from the baked cone via the b18 coverage
  probe, which now serve-stales too — verified: all five spectra return
  200 offline. If the orchestrator wants the spectrum echo snapped as
  well, that is a one-line call to the new `snap_to_preset` in
  `spectrum_params` — out of scope here.
- **TAP fallback unchanged**: `refresh_search_body` goes through
  `sia_body`, so the per-collection disk write path is identical to a
  normal search; `transient_failure` is the same predicate the SIA→TAP
  fallback already used (renamed only — `sia_failed` was only called
  there and in the new code path, behaviour identical).
- **Negative cache untouched**: memory-only failure TTLs and the
  `negative_cached` logic were not modified; no disk entry +
  dead upstream still yields 502.

## Open issues

- None blocking. Minor: `disk_observations`'s stale-refetch path makes
  one SIA request per collection (same as a normal search); under a
  dead upstream each attempt costs the retry budget (~2 × 0.3 s
  measured per collection — visible in the timings below).

## Verification — gates (all from `backend/`)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test` | all green, **0 ignored** (incl. 9 new preset_snap tests) |
| `cargo test --release --test perf_tests` | 7/7 ok (12.94 s) |
| `cargo deny check licenses advisories bans` | ok |
| `bash scripts/docker-smoke.sh spherex-blink:b20` | all checks passed, image 177 MiB |

New tests (`backend/tests/preset_snap_tests.rs`, loopback axum stub,
synthetic VOTable/FITS — zero real IRSA): distance wrap/poles, snap
boundaries (exact / 0.05" / 0.5" / fov ±0.0005–0.002 / non-finite
passthrough), all five presets end-to-end through the real router +
`IrsaClient` for exact/sexagesimal/7-6-5-4-decimal/offsets asserting
echo values **and** that the stub's upstream counter stays flat on
re-requests, fov mismatch not snapping, NaN/inf → 400 before any
upstream hit, stale+503 → 200 from disk, stale+upstream-ok → refreshed
body served, empty-dir+dead → 502, and the radius-vs-rounding
invariant.

## Verification — real baked cache (no new IRSA downloads)

Setup: `cp -r` of `../b17-bake-verify/cache-bake` (455 files),
`search-*.xml` (10 bodies) aged 3 days with `touch -d`, release binary
with `IRSA_BASE_URL`/`SPHEREX_S3_BASE_URL`/`IRSA_TAP_URL` →
`http://127.0.0.1:1` (dead), `CACHE_DIR=<aged copy>`.

**All five presets × {exact decimals, frontend sexagesimal strings,
7/6/5/4-decimal roundings} → HTTP 200 with 22–24 frames; both
`/api/v1/spectrum` forms → 200 with 12–24 points.** Echo check: every
non-exact request's `query.ra_deg/dec_deg/fov_deg` equals the preset
constants — the snap is visible in the response. Timings: first request
per preset 1.7–4.0 s (stale refetch attempts + disk reads + reproject),
repeats 0.15–0.40 s; spectra 2.0–5.5 s first, ~1 ms cached. A
non-preset position (`ra=10&dec=-20`) correctly returns 502. Server log
shows the sanitised warning:

```
WARN spherex_backend::irsa::client: disk cache: serving stale search
  body after upstream failure (unavailable after 3 attempt(s): connect
  error (host 127.0.0.1))
```

**Docker `--network none`, entrypoint bypassed** (`docker run -d
--network none spherex-blink:b20 /app/spherex-backend`, so the mtime
refresh never ran — the baked files in the image were already 3 days
old): 35 checks inside the container (5 presets × exact + sexagesimal
+ r7/r6/r5/r4 + 5 spectra) → **all 200, FAILURES: 0**; 10
`serving stale` warnings in the container log. `docker exec` was used
for curl since `--network none` drops port publishing.

Container and all temp files removed; `cache-bake/` copy deleted after
the image build (it is regenerated by `scripts/bake-cache.sh`).

## Merge

```sh
git switch main && git merge --no-ff devin/b20-preset-snap-stale
```
