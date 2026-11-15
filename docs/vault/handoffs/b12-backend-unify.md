# Handoff: b12-backend-unify

Status: **done**. One branch, `devin/b12-backend-unify`, ready to merge into
`main`. All backend work unified; `frontend/` byte-identical to `main`.

## How the tree was composed

No `git merge` was used (the integration branch contains the frontend
commit `765a03d`, which must not come in). Everything was taken
path-wise:

```sh
# integration-live only touched these paths outside frontend/ (verified
# against merge-base ea20f6d); its backend already carries the b6
# security/fuzz battery adapted to the new code.
git checkout devin/b7-fix-api-bugs -- backend          # = integration-live backend + b7 fixes
git checkout devin/b11-presets-epoch -- \
    backend/src/api/targets.rs backend/tests/api_tests.rs backend/tests/targets_tests.rs
git checkout devin/b8-science-validation -- \
    backend/tests/science_validation_tests.rs backend/tests/support/scenes.rs \
    docs/vault/handoffs/b8-science-validation.md
git checkout devin/b9-perf-tests -- \
    backend/tests/perf_tests.rs backend/tests/support/perf.rs \
    docs/vault/handoffs/b9-perf-tests.md
git checkout opus/integration-live -- scripts/warm.sh docs/vault/handoffs/integration-live.md
git checkout devin/b7-fix-api-bugs  -- docs/vault/handoffs/b7-fix-api-bugs.md
git checkout devin/b11-presets-epoch -- docs/vault/handoffs/b11-presets-epoch.md
```

- b7 files and b11 files are disjoint (b7: fetch/frames/mod/params/
  spectrum/config/irsa+error/fuzz/security; b11: targets/api_tests/
  targets_tests) — zero textual conflicts.
- `docs/REFERENCES.md`: b11's `git apply` failed (main's file grew; the
  hunk context also differs, `µm` vs `um`). The b11 citations (SIMBAD +
  epoch-2026.0 astrometry) were applied by hand onto main's newer file.
- `SI_USAGE.md`: kept main's table (already covers integration-live);
  added one row for this task. integration-live's own row was subsumed.
- `docs/vault/`: kept main's newer state/decisions/handoffs; only the
  five source-task handoffs were added. integration-live's vault view
  was stale (based on an older main), so it was not checked out.

## Conflict / adaptation decisions

- `AppState.soft_deadline` (new field from integration-live) added to
  the b9 `perf_state` helper — the documented E0063 fix.
- Perf budgets re-based on the Opus **native-scale grid**: the handler
  now picks `ceil(fov/native_scale*4/8)*8` clamped to `[32, OUTPUT_NPIX]`
  and then capped by `payload_npix_cap` (120 px at fov=0.05 with the
  ~6.2"/px stub cutouts; 255 px at 60 frames/256 cap). The b9 e2e tests
  silently measured a 120 px workload while labelled `@192`/`@256`:
  - `perf_4` payload-via-router, `perf_6` memory and `perf_7`
    concurrency now request `fov=0.5` (`FOV_WIDE_DEG`) so the
    `OUTPUT_NPIX`/payload caps really bind — the worst case is still
    measured, not assumed.
  - `perf_5` keeps the typical `fov=0.05` request and is honestly
    labelled `@120`; budgets re-set from measurement (~90 ms median on
    this box → 350 ms raw / 450 ms others, same ~4x headroom style as
    the rest of the suite).
  - New helpers in `tests/support/perf.rs`: `body_width` (reads the
    `width` field the API reports) and `expected_out_npix` (mirrors
    `output_npix` + `payload_npix_cap`); every e2e test now asserts the
    returned raster side, so grid-selection regressions fail the suite.
- Stale comment fix in `fuzz_tests.rs`: the b6-bug block is no longer
  `#[ignore]`d (b7 enabled it); comment updated.

## Deploy/env alignment (code is source of truth)

`docs/DEPLOY.md`, `docker-compose.yml` and `scripts/container.env.example`
now list every variable read by `config.rs`/`irsa/config.rs` with the
code's defaults: `REQUEST_SOFT_DEADLINE_SECS=8`, `IRSA_TIMEOUT_SECS=30`
(was documented as 15 — code default changed in integration-live),
`IRSA_TOTAL_TIMEOUT_SECS=300`, `IRSA_FAILURE_TTL_SECS=60`,
`CUTOUT_SOURCE=s3`, `SPHEREX_S3_BASE_URL`, `S3_MAX_CONCURRENCY=16`,
`CACHE_DIR` (unset = memory-only). Compose gained a
`CACHE_DIR=/data/cache` named-volume example; the Dockerfile pre-creates
`/data/cache` app-owned so the volume is writable (disk cache degrades
to warn-only if it is not). `Dockerfile`/`docker-compose.yml`/
`docs/DEPLOY.md`/`scripts/container.env.example` were otherwise kept
from main — Opus did not change them.

## Gate results (this worktree)

- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test` (debug): **151 passed, 0 failed, 0 ignored** —
  api 25, fits 14, fuzz 12, irsa 25, perf 7, science 15,
  science_validation 12, security 28, targets 5, validation 8.
- `cargo test --release --test perf_tests` (release): **7 passed**;
  measured medians: e2e `/frames` 24 fr @120 px ≈ 90 ms/mode;
  60 fr @255 px payload 20.8 MB raw / 225 KB gzip; 8x concurrent @192 px
  398 ms vs 1786 ms sequential, VmHWM +57 MiB.
- `bash scripts/docker-smoke.sh spherex-blink:unify`: **all 13 checks
  green** (health, presets, SPA+fallback, gzip, security headers, 404/
  400 JSON errors, uid 999, docker healthcheck, TLS to IRSA, graceful
  SIGTERM 128 ms). Image 140 MiB. Container removed by the script.
- `git diff main -- frontend | wc -l` → **0** (frontend identical).
- `git diff main -- shared | wc -l` → **0** (contract untouched).

## Deviations / notes

- `scripts/warm.sh` is included but not covered by the gates above; it
  is an operator script from integration-live (see its handoff).
- Compose now defaults `CACHE_DIR=/data/cache` with a named volume —
  persistent cache on by default *in compose only*; the app default is
  still memory-only.
- The `main` checkout and other branches were never touched; nothing
  was pushed.

## To merge (user)

```sh
git switch main && git merge --no-ff devin/b12-backend-unify
```
