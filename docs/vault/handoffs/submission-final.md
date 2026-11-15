# Handoff: submission-final — submission texts refreshed with measured facts

Status: **done**. Every submission text now carries only facts traceable to
`docs/live-results/*.json` or a vault handoff. `TODO(team):` markers remain
only for team name, member names, the demo/final/repository links and the
idea-authorship statement (open question in `docs/vault/open-questions.md`,
unchanged — authorship left to the team). No `TODO(measured):` left.

## Files

- `README.md` — rewritten: three real screenshots linked from
  `docs/screens-real/`, the five presets table (epoch-2026.0 coordinates
  from `backend/src/api/targets.rs`), "How the data flows" (SIA2+TAP
  search, S3 byte-range cutouts with IBE fallback, memory+disk caches,
  baked cache, preset snap), offline quick start
  (`docker run --network none`), full URL-parameter table (`ra`, `dec`,
  `fov`, `mode`, `stretch`, `cmap`, `fps`, `frame`, `det`, `embed`,
  `mock`, `hint` — verified against `urlState.ts`, `App.tsx`,
  `FirstRunHint.tsx` on `opus/f10-real-data-qa`), verified-results section,
  honest limitations (WISE 0855 faint/undetected, diff needs comparable
  exposures, IRSA slow/502 → baked demo), test counts, Apache-2.0.
- `docs/submission/project-page.md` — High-Level Summary rewritten and
  recounted (**108 words**); Detailed Description updated with the S3/TAP
  data path, a per-preset measured table (frames, diff frames, wavelength
  range, detectors, first frame baked), the proper-motion checks, image
  size and test counts; SI field rewritten to list exactly what each SI
  tool did + the `TODO(team)` idea-authorship block (verbatim intent of
  the open question); NASA Data gained the `nasa-irsa-spherex` AWS Open
  Data bucket; Aladin Lite entry notes the jsDelivr fallback; dependency
  list regenerated (see below); embed-URL example now uses the
  epoch-2026.0 Barnard's Star preset.
- `docs/submission/checklist.md` — summary count updated (108), added
  repository-link and baked-cache checklist items; deadline/FortiGuard/
  checkbox content kept (still accurate today).
- `docs/submission/demo-slides.md` — all `TODO(measured)` image
  placeholders replaced with real `../screens-real/` files; slide 4 now
  reports the measured proper motion and the honest WISE 0855
  non-detection; speaker notes re-verified ≤ 40 words (max: 40).
- `docs/submission/demo-video-script.md` — capture plan switched to the
  Barnard's Star preset with real captions ("24 of 390 epochs · measured
  8.9″/yr (catalogue 10.4″/yr)"); honest WISE 0855 alternative noted;
  stale `TODO(measured)` items removed.
- `SI_USAGE.md` — added rows: f9 frontend real-data integration (Opus
  5.5), f10 real-data QA (Opus 5.5), b17 bake/verify (Devin), b20
  preset-snap/serve-stale (Devin), this task (Devin). The idea-authorship
  `TODO(team)` stays.
- `docs/screens-real/` — the 13 QA JPEGs checked out from
  `opus/f10-real-data-qa` and committed (task instruction).

## Fact provenance

| Claim | Source |
|---|---|
| Preset coordinates / FOVs / 0.3″ snap | `backend/src/api/targets.rs`, `params.rs`; handoff b11, b20 |
| Frames/diff/obs per preset, λ range, detectors, MJD, timings | `docs/live-results/*.json` (b17 probe, `--network none`) |
| First frame 0.29–0.74 s headless / ≤ 1 s browser | f10 handoff, Performance table |
| Proper motion 8.9 vs 10.4 (Barnard), 3.3/2.7 vs 4.0/2.5 (ε Indi B), WISE 0855 undetected (3–7σ) | f10 handoff "Orientation (real data)" + preset table |
| Image 177.9 MiB baked / 139.5 MiB bare, build ~36 s, 13-check smoke, uid 999 | b17, ops-verify-image, b20 handoffs |
| Cold IRSA ~1–13 min per preset; baked 0.2–3.7 s | b17 handoff |
| IBE cutout ~4 MB / ~97 % EPSF / ~55 KB/s; S3 byte-range bit-identical | integration-live (f9) handoff |
| Concurrency 3, retries, deadline 40 s/soft 8 s, 24-frame selection | D-014, integration-live handoff |
| Backend 194 tests = 185 (b19) + 9 new (b20); frontend 250 vitest, 62 Playwright + 2 `@live` skipped | b19/b20/f10 handoffs |
| URL params `det`, `mock`, `hint` | `urlState.ts`, `App.tsx`, `FirstRunHint.tsx` on `opus/f10-real-data-qa` |
| Dependency list | re-run of `list-deps.sh` (see below) |

## Decisions / deviations

- **npm lockfile:** `frontend/package-lock.json` on this branch is
  byte-identical to `opus/f10-real-data-qa`'s (`git diff` empty), so
  `list-deps.sh` ran on the worktree file directly — same result as
  required. Regenerated output: 571 lines; new entries vs the previous
  paste are the Playwright/axe-core family (f3 e2e deps, now locked).
- **`devin/f11-polish-real-data` exists but has no handoff file** — its
  commits are the f10 set; nothing extra was sourced from it.
- **Backend test count is arithmetic** (185 + 9), not a fresh `cargo
  test` run; both addends come from handoff gate tables.
- **WISE 0855 diff = 11 frames** (the curl/`live-results` number); the
  browser saw 14 after polling — the conservative measured value is used.
- **Slide 2's "dry FITS/archive" image** has no shipped screenshot; it is
  left as a plain suggestion (no TODO marker) so the marker rule holds.
- Example URLs switched from the J2000 challenge coordinates
  (`08h55m10.831s -07d14m42.53s`) to the epoch-2026.0 preset coordinates —
  the old ones are ~2′ off the moving targets and would not hit the baked
  cache (snap radius is 0.3″).
- No code, contract, Docker or `docs/REFERENCES.md` changes (out of
  scope).

## Open issues (not in scope, reported not fixed)

- f10 O1: WISE 0855 raw ends "Partial: 22 of 24" on baked data (2
  permanent edge-of-array 422s; frontend can't distinguish filling vs
  done — needs a contract `complete` flag, orchestrator decision).
- f10 O5: a WISE 0855 raw request takes ~3.7 s offline because the
  backend probes dead upstreams for missing pieces first; an unbaked
  offline spectrum waits out the 40 s deadline.
- f10 O3/O4: 3 px spectrum aperture is jumpy on fast movers; flagged
  outliers crush the y-axis. Design decisions, deferred.
- Repo link still `TODO(team)` — About dialog link and checklist item
  wait for the public repo URL.

## Verify commands and results

```sh
# dependency list regenerated
bash docs/submission/list-deps.sh > /tmp/deps.txt   # exit 0, 571 lines
# pasted into project-page.md between the two '###' headers (verified)

# relative-link check over README + docs/submission/*.md
# (python: extract [...](...) and `../`/`docs/` refs, resolve vs file dir)
#   -> "missing links: NONE"

# High-Level Summary word count (copy block only)
#   -> 108 words (limit 120); speaker notes max 40 words

grep -rn "TODO(measured)" README.md docs/submission/ SI_USAGE.md
#   -> only the checklist line that mentions the marker convention itself
```

Gates for code (cargo/npm) were not re-run — no `backend/` or
`frontend/` files were touched; test counts are quoted from handoff gate
tables, not re-executed.

## Merge

```sh
git switch main && git merge --no-ff devin/submission-final
```
