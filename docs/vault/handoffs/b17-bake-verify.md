# Handoff: b17-bake-verify — baked preset cache + offline proof

Status: **done**. All five presets are baked into `cache-bake/` (30 MB,
455 files), shipped in the image at `/data/cache`, and verified end to end
with `docker run --network none` — real frames, diff rasters and spectra
with zero upstream traffic.

## What was built

- `scripts/bake-cache.sh` — builds the release backend, starts it with
  `CACHE_DIR=./cache-bake WARM_TARGETS=1`, waits for the
  `cache warm-up finished` log line, then drives
  `scripts/probe-presets.sh --poll` so every preset's `/frames` raw+diff
  and `/spectrum` responses are complete on disk. Sequential requests only;
  the backend's own `IRSA_MAX_CONCURRENCY=3` + retry/backoff bounds IRSA
  load. Global budget `BAKE_MAX_SECS` (default 2400 s); on timeout it
  reports which presets are incomplete and exits 1 — nothing is faked.
- `scripts/probe-presets.sh` — probes all five presets on a running
  server (plain HTTP or `--docker NAME` for `docker exec` curl, which is
  how the `--network none` run was queried) and writes
  `docs/live-results/<preset>.json`: frame/diff/spectrum counts,
  wavelengths, detectors, MJD span, truncated flag, response times and a
  proper-motion check (brightest-pixel centroid inside the central
  quarter-radius, same-detector wavelength-matched epoch pairs,
  frames >30 % NaN discarded).
- `scripts/docker-entrypoint.sh` — new container `CMD`: refreshes the
  mtimes of every file under `CACHE_DIR` at start and hourly, then
  `exec`s the backend as PID 1. See "The 24 h SIA freshness issue" below.
- `Dockerfile` — new stage `baked-cache` copies `cache-bake/` into
  `/data/cache` (`--chown=app:app`); `ENV CACHE_DIR=/data/cache`;
  `CMD ["/app/docker-entrypoint.sh"]`. The stage goes through
  `COPY . .` + conditional `cp`, so `docker build` still works when
  `cache-bake/` is absent (empty `/data/cache`, ordinary cold cache).
- `.dockerignore` — `cache-bake/` is NOT excluded; `scripts/` became
  `scripts/*` + `!scripts/docker-entrypoint.sh` so only the entrypoint
  reaches the context.
- `.github/workflows/ci.yml` — carried over from
  `devin/b16-docs-contract-deploy` via `git checkout`; `git diff`
  confirmed the only change vs this branch is the `supply-chain` job
  (cargo-deny, `backend/deny.toml` exists). YAML validated with
  `yaml.safe_load`.
- `docs/DEPLOY.md` — new "Baked preset cache" section, updated
  `CACHE_DIR` default row and measured image size.
- `docs/live-results/*.json` — per-preset offline probe summaries.

## What is cached (from `backend/src/irsa/client.rs`)

- `search-<fnv>.xml` — raw SIA VOTable body per collection (and `tap`
  suffix for the TAP fallback). Read through `disk_cached_get` /
  `disk_observations` with `max_age = Some(SEARCH_DISK_TTL)` =
  **24 h from file mtime**.
- `cutout-<fnv>.fits` — EPSF-stripped cutout FITS (~4 MB → ~0.1-0.4 MB).
  `disk_read(..., max_age = None)`: **never expires**.
- `*.meta` — coverage-index sidecars, no age check.
- Keys hash the exact f64 bits of (collections, ra, dec, radius/fov, obs
  id) — requests must use the exact preset decimals from
  `/api/v1/targets` (the probe does).
- Failures are negative-cached in memory only (`IRSA_FAILURE_TTL_SECS`
  60 s transient, `PERMANENT_FAILURE_TTL` 1 h deterministic); they do not
  poison the disk cache.

## The 24 h SIA freshness issue (verified, worked around, recommended fix)

`SEARCH_DISK_TTL` (client.rs ~line 255) is hardcoded to 24 h, far short of
the judging window. Worse, when a search body is stale the code refetches
upstream and has **no serve-stale fallback**:

- `IrsaClient::disk_cached_get` (client.rs ~line 1183): `disk_read(path,
  Some(SEARCH_DISK_TTL))` → on stale it calls `get_with_retry` and returns
  its error directly.
- `IrsaClient::disk_observations` (~line 1138, used by the b18 spectrum
  coverage probe): same TTL gate.

**Verified by simulation**: copy of the cache with `search-*.xml` aged 2
days + `IRSA_BASE_URL`/`SPHEREX_S3_BASE_URL`/`IRSA_TAP_URL` pointed at a
dead port → `/api/v1/frames` returns **502 upstream_unavailable** in 0.3 s.
Same setup with fresh mtimes → all five presets serve from disk (also
verified in the container with `--network none`).

**Workaround shipped** (in scope, no `backend/src` edit):
`scripts/docker-entrypoint.sh` refreshes file mtimes at container start
and hourly, so the baked search bodies are always inside the 24 h window
for the container's lifetime.

**Recommended backend fix for the orchestrator** (not a blocker): in
`disk_cached_get` and `disk_observations`, on upstream error retry
`disk_read(path, None)` before returning the error (serve-stale), or make
`SEARCH_DISK_TTL` configurable / skipped for baked entries.

## Results — offline run (`docker run --network none`)

Image `spherex-blink:baked` = **177.9 MiB** (139.5 MiB without the cache;
baked payload 30 MB < 100 MB target). Probe output per preset
(full details in `docs/live-results/<preset>.json`):

| preset | raw | diff | total obs | spectrum pts | λ range (µm) | detectors | MJD span | offline first (s) | offline repeat (s) | spectrum (s) |
|---|---|---|---|---|---|---|---|---|---|---|
| barnards-star | 24 | 13 | 390 | 23 | 0.74–4.97 | 1–6 | 60902.9–61305.6 | 0.61 | 0.62 | 3.4 |
| epsilon-indi-b | 24 | 15 | 488 | 22 | 0.87–4.76 | 1–6 | 60789.9–61315.5 | 0.55 | 0.55 | 3.1 |
| wise-0855 | 22 | 11 | 280 | 21 | 0.80–4.63 | 1–6 | 60800.7–61174.3 | 3.67 | 0.51 | 3.1 |
| rw-aurigae | 24 | 18 | 341 | 24 | 0.76–4.73 | 1–6 | 60919.4–61299.9 | 0.22 | 0.25 | <0.01 |
| r-draconis | 24 | 14 | 565 | 12 | 0.74–4.84 | 1–6 | 60812.0–61237.9 | 0.23 | 0.22 | 3.2 |

Cold vs baked: during the live bake (IRSA answering 200 but slowly,
6–27 s per call, several SIA 502s and S3 timeouts) the default raw
selection took on the order of **1–13 min per preset** to complete
(warm-up: 3 presets / 65 frames in 16.3 min; rw-aurigae needed ~13 min
through repeated upstream 502s). From the baked cache with **no network**
the same requests answer in **0.2–3.7 s** (first request pays the disk
read + reproject; repeats ~0.2–0.6 s). That is the demo story.

Proper motion (crude brightest-pixel centroid, wavelength-matched
same-detector pairs — see the method note in `probe-presets.sh`):
- **barnards-star**: measured **8.4″ shift over 0.60 yr → ~14″/yr**
  (expected 10.3″/yr; an earlier pairing measured 10.25″ over 1.07 yr →
  9.6″/yr). Motion is real and clearly measurable; centroid method has
  ~±1–2 px error.
- **wise-0855**: **not detectable** — measured shifts are noise/source-
  confusion level (0.7″ and 133″/yr depending on the pair); the Y dwarf
  is too faint for this estimator. Expected 8″/yr.
- **epsilon-indi-b**: not reliably measurable by centroid — the
  brightest in-field source changes identity between epochs
  (field crowded by ε Indi A); measured values are artifacts.
- rw-aurigae / r-draconis: expected ~0.02″/yr; measured 3.3″/yr and
  3.1″/yr — consistent with estimator noise (≈2 px).

Notable data facts: 280–565 observations per preset across
`spherex_dr1` + `spherex_qr3`; `truncated=true` everywhere because
`total_observations` (280–565) exceeds the 24-frame selection — by
contract design, not an error. wise-0855 raw serves 22/24: two selected
cutouts failed permanently upstream (edge-of-array 422 class); diff=1
during the bake poll grew to 11 once all downloads landed.

## Rebuild / verify commands

```sh
scripts/bake-cache.sh                 # incremental; BAKE_FRESH=1 for clean
# Budget/IRSA tuning (used for this run):
BAKE_MAX_SECS=5400 IRSA_TIMEOUT_SECS=45 IRSA_TOTAL_TIMEOUT_SECS=600 \
  scripts/bake-cache.sh

docker build -t spherex-blink .       # works with or without cache-bake/
docker run -d --name off --network none -e RATE_LIMIT_RPS=16 spherex-blink
scripts/probe-presets.sh --docker off docs/live-results
scripts/docker-smoke.sh spherex-blink # 13 checks, incl. graceful stop
```

## Commit-vs-CI proposal for `cache-bake/` (decision needed)

`cache-bake/` is **left untracked** (30 MB, 455 files) per the task. My
recommendation: **commit it once** (`git add cache-bake/`) — the entire
point is that the demo must not depend on IRSA being up, and rebuilding
it in CI reintroduces exactly that dependency (a ~10–40 min live-IRSA
job that can 502 on a bad day). 30 MB is a one-time git cost; refresh it
rarely (`BAKE_FRESH=1 scripts/bake-cache.sh` + recommit) and let
`.dockerignore` keep shipping it. If the repo must stay lean, the
alternative is a CI job that runs the bake before `docker build` and
accepts that a failed bake produces an unbaked image (build still passes
— the COPY is tolerant by design).

## Open issues / notes

- wise-0855 serves 22/24 raw frames (2 selected observations fail
  permanently upstream — edge-of-array 422 class; honest partial result).
- `rw-aurigae` spectrum answered in <1 ms on one run: real 200 with 24
  points, fully memory-cached.
- `SI_USAGE.md` is outside my allowed scope and was not updated — the
  orchestrator should record that `bake-cache.sh`, `probe-presets.sh`,
  `docker-entrypoint.sh`, the Dockerfile/.dockerignore/DEPLOY.md edits,
  the ci.yml carry-over and `docs/live-results/*.json` are SI-generated.
- The hourly mtime refresh in the entrypoint keeps baked SIA bodies
  fresh indefinitely; the stale-refetch-without-fallback behavior above
  remains the recommended backend hardening (file + functions named).
- Containers/images cleaned up (`spherex-offline`, smoke container,
  `spherex-blink:nocache` test image removed); temp dirs under /tmp
  removed except the bake server log `/tmp/spherex-bake.HLfa1h.log` and
  probe results `/tmp/spherex-bake-results.ANrrKo/` kept for reference
  until merge.
