# Handoff: b19-backend-final-unify

Status: **done**. Branch `devin/b19-backend-final-unify` (stacked on
`devin/b12-backend-unify`) composes the final backend. `frontend/` is
byte-identical to `main` (`git diff main -- frontend | wc -l` -> **0**).

## How the tree was composed

Same method as b12: no `git merge`, everything path-wise. Branch bases:
b13/b14/b15/b16 on `devin/b12-backend-unify`, b18 on
`devin/b14-spectrum-latency` (its `spectrum.rs` already contains b14's
changes, so checking out b18's file yields b14+b18).

```sh
# b13 (sia.rs, frames.rs, tests) — disjoint files, direct checkout.
git checkout devin/b13-diff-selection -- \
    backend/src/irsa/sia.rs backend/src/api/frames.rs \
    backend/tests/diff_selection_tests.rs \
    docs/vault/handoffs/b13-diff-selection.md

# b14 tests + handoff (spectrum.rs itself comes from b18 below).
git checkout devin/b14-spectrum-latency -- \
    backend/tests/spectrum_latency_tests.rs \
    docs/vault/handoffs/b14-spectrum-latency.md

# b15 (health.rs, api/mod.rs, irsa/{client,config,mod,tap}.rs, tests).
git checkout devin/b15-sia-resilience -- \
    backend/src/api/health.rs backend/src/api/mod.rs \
    backend/src/irsa/client.rs backend/src/irsa/config.rs \
    backend/src/irsa/mod.rs backend/src/irsa/tap.rs \
    backend/tests/sia_resilience_tests.rs \
    docs/vault/handoffs/b15-sia-resilience.md

# b18 spectrum.rs (= b14+b18) plus its tests and handoff.
git checkout devin/b18-spectrum-cache-reuse -- \
    backend/src/api/spectrum.rs \
    backend/tests/spectrum_cache_reuse_tests.rs \
    docs/vault/handoffs/b18-spectrum-cache-reuse.md

# b16 (deny.toml, DEPLOY, REFERENCES, compose, env example, contract,
# handoff). The branch also touches .github/workflows/ci.yml — OUTSIDE
# this task's allowed scope, so it was NOT applied (see Deviations).
git checkout devin/b16-docs-contract-deploy -- \
    backend/deny.toml docs/DEPLOY.md docs/REFERENCES.md \
    docs/vault/handoffs/b16-docs-contract-deploy.md \
    docker-compose.yml scripts/container.env.example \
    shared/openapi-contract.json

# b18's additions onto the b15 versions of the two overlapping files.
git diff devin/b14-spectrum-latency..devin/b18-spectrum-cache-reuse -- \
    backend/src/irsa/client.rs | git apply -3
git diff devin/b14-spectrum-latency..devin/b18-spectrum-cache-reuse -- \
    backend/src/api/mod.rs | git apply -3
```

## Conflict decisions (all additive — keep both)

- `backend/src/api/mod.rs` (2 hunks): b15's `FrameSource::upstream_health`
  + `health` module and b18's `cached_cutout`/`cached_observations`
  trait methods with their `IrsaClient` impls — all kept.
- `backend/src/irsa/client.rs` (4 hunks):
  - imports: union (`{Arc, Mutex, PoisonError}` +
    `{HashMap, VecDeque}`).
  - `struct Inner` and its constructor: both fields kept
    (`health: Health` and `index: Mutex<CoverageIndex>`).
  - helper block at EOF: both function sets kept — b15's `PROBE_POS`,
    `sia_failed`, `contact_ok` plus b18's `disk_write_meta`,
    `sky_dist_deg`, `frame_covers`, `Meta`, `parse_meta`.
- Cleanly merged (no conflict) and verified: b18's `index_search`/
  `index_cutout` calls landed correctly inside b15's refactored
  `search`/`fetch_cutout`, and `disk_write_meta` inside b15's shared
  `disk_cached_get` (the 3-way apply handled these regions).
- `docs/vault/handoffs/`: each branch adds its own file — no
  conflicts.

## Orchestrator decisions applied

1. **`HEALTH_PROBE_SECS` default = 300** (was 60): changed in
   `IrsaConfig::default`, the `irsa/config.rs` doc table and field doc,
   `docker-compose.yml`, `scripts/container.env.example`,
   `docs/DEPLOY.md`, and noted in the b15 handoff. No test asserted the
   old default (the test config sets `from_millis(150)` explicitly).
   Rationale recorded in the env comments: the container HEALTHCHECK
   polls `/health` every 30 s, so 300 s keeps idle probes rare.
2. **Contract `Health` schema**: added OPTIONAL `upstream_ok`
   (boolean) and `upstream` (object with `last_sia_ok`, `last_tap_ok`,
   `last_ibe_ok`, `last_s3_ok`, each `type: integer, nullable: true`)
   with descriptions matching the b15 handoff; `required` unchanged.
   Verified additive-only: stripping `description` fields and the
   `Health` schema from both files makes this branch's contract JSON
   identical to main's (python json comparison: `True`).
3. **Env docs re-extracted** from `backend/src/config.rs` and
   `backend/src/irsa/config.rs`: the three new vars
   (`SIA_FALLBACK_TAP`, `IRSA_TAP_URL`, `HEALTH_PROBE_SECS`) are now in
   the `docs/DEPLOY.md` table, `docker-compose.yml` `environment:`
   block and `scripts/container.env.example`. No other variable was
   missing (b18 added no env vars; verified by grepping all
   `std::env::var`/`env_u64`/`env_usize` reads).
4. **`docs/REFERENCES.md`**: added the IRSA TAP service
   (`https://irsa.ipac.caltech.edu/TAP`) and the "Spectral Image
   Cutouts" notebook (`https://caltech-ipac.github.io/irsa-tutorials/spherex-cutouts/`,
   the ADQL pattern source per the b15 handoff).
5. **`scripts/docker-smoke.sh`**: the health check now also greps
   `"upstream_ok":` and `"upstream":{` (in scope: `scripts/`).
6. **`SI_USAGE.md`**: added rows for the checklist tasks and this
   unification.

## Gate results (this worktree, 2026-11-14/15)

- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test` (debug): **185 passed, 0 failed, 0 ignored** — api 25,
  diff_selection 10, fits 14, fuzz 12, irsa 25, perf 7, science 15,
  science_validation 12, security 28, sia_resilience 10,
  spectrum_cache_reuse 6, spectrum_latency 8, targets 5, validation 8.
- `cargo test --release --test perf_tests`: **7 passed** (12.98 s).
- `cargo deny check licenses advisories bans`: `advisories ok, bans
  ok, licenses ok` (cargo-deny 0.20.2, `backend/deny.toml`).
- `bash scripts/docker-smoke.sh spherex-blink:final`: **all checks
  green**, including the new `upstream_ok`/`upstream` health check;
  140 MiB image, uid 999, healthcheck healthy, SIGTERM graceful in
  143 ms. Smoke container removed by the script; `docker ps -a` shows
  no leftovers.
- `git diff main -- frontend | wc -l` -> **0**.

## Deviations / open issues

- **`.github/workflows/ci.yml` not carried over** (outside allowed
  scope): b16's `supply-chain` CI job (12 lines adding
  `EmbarkStudios/cargo-deny-action@v2` with
  `manifest-path: backend/Cargo.toml`,
  `command: check licenses advisories bans`) is NOT in this branch,
  while `docs/DEPLOY.md` (in scope, taken from b16) describes it.
  Apply `git checkout devin/b16-docs-contract-deploy --
  .github/workflows/ci.yml` at merge time or merge b16's CI hunk
  separately. `deny.toml` itself IS included, and `cargo deny` passes
  locally.
- The b15/b18 conflict resolution kept both feature sets verbatim; the
  semantic merge was verified by the full suite (185 tests, incl. the
  10 sia_resilience and 6 spectrum_cache_reuse tests which exercise
  both sides end to end).
- `HEALTH_PROBE_SECS=300` is a default change only; the env var still
  overrides.
- Known from source handoffs (not regressions): TAP latency (~2 min)
  can exceed `REQUEST_DEADLINE_SECS` during an SIA outage; the probe
  exercises SIA only; `spherex_*_deep` TAP release-path layout is
  assumed, unverified live.
- `main` checkout and other branches untouched; nothing pushed.

## To merge (user)

```sh
git switch main && git merge --no-ff devin/b19-backend-final-unify
# then, for the b16 CI job (out of this task's scope):
git checkout devin/b16-docs-contract-deploy -- .github/workflows/ci.yml
```
