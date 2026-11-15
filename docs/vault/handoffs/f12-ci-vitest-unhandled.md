# f12-ci-vitest-unhandled — handoff

Status: done. Vitest unhandled error fixed at the jsdom boundary; CI backend
jobs gated on `backend/Cargo.toml`; all gates green.

## Root cause (verified, deterministic)

- `node_modules/uplot/dist/uPlot.cjs.js:2950`: `new uPlot()` stores
  `self.ctx = can.getContext("2d")`. jsdom has no canvas backend, so ctx is
  `null`.
- The constructor queues `_commit` via `microTask(_commit)`
  (uPlot.cjs.js:4762, `queueMicrotask` at :752).
- `_commit` (uPlot.cjs.js:4888) dereferences `ctx.clearRect` →
  `TypeError: Cannot read properties of null (reading 'clearRect')` as an
  uncaught microtask error.
- Flakiness: `SpectrumPanel` loads uPlot through a dynamic `import()`. If the
  import resolves before the panel unmounts / the test ends, the chart is
  built and the queued commit throws inside or after the test; if it resolves
  after, the `disposed` guard skips `buildChart` entirely. Local runs almost
  always take the second path; the CI runner took the first.
- After giving the canvas a working ctx, the next jsdom gap surfaced
  deterministically: uPlot builds series paths with `new Path2D()`
  (uPlot.cjs.js:1900+) → `ReferenceError: Path2D is not defined`.

## Files

- `frontend/tests/setup.ts` (new): per-canvas no-op 2D context via
  `WeakMap` + `Proxy` (`measureText` → `{width:0}`, gradients,
  `createImageData`/`getImageData` → real-shaped `{width,height,data:
  Uint8ClampedArray,colorSpace}`; every other member a no-op). Non-2D
  contexts still return `null`, so `createFrameRenderer`'s
  WebGL2 → Canvas2D → Null fallback is unchanged. Also stubs `Path2D` as a
  construct-trap Proxy.
- `frontend/vite.config.ts`: `defineConfig` now from `vitest/config`;
  added `test.setupFiles: ["tests/setup.ts"]`.
- `frontend/tests/integration/app.test.tsx`: removed the per-file
  `getContext → null` stub (it would have re-broken uPlot). The frame
  renderer now exercises the Canvas2D path instead of NullRenderer;
  assertions unaffected (DOM roles only), no "Not implemented" noise.
- `frontend/tests/spectrum/uplot-late-commit.test.tsx` (new): regression
  test. Mounts the real `SpectrumPanel`, waits for `.uplot` in the DOM
  (constructor ran ⇒ commit queued), unmounts, flushes micro/macrotasks
  and asserts `uncaughtException`/`unhandledRejection` listeners collected
  nothing.
- `.github/workflows/ci.yml`: new `detect` job outputs `has_backend`
  (`[ -f backend/Cargo.toml ]`); `backend`, `supply-chain` and `docker`
  jobs get `needs: detect` + `if: needs.detect.outputs.has_backend ==
  'true'`. Frontend job unconditional. YAML validated with
  `yaml.safe_load`.
- `.github/workflows/e2e.yml`: untouched (no backend-dependent jobs).

## Deviation / decisions

- Task preference order followed: fix at test level (setup file). No
  source guard was needed in `src/components/spectrum/` — with a real
  (no-op) ctx, a late `_commit` is harmless even on a destroyed chart.
- `Path2D` stub added on top of the ctx stub: required, otherwise the
  commit crashes one frame later.
- `process.on` used via structural typing (`globalThis.process`) because
  `@types/node` is not a dependency.
- `SI_USAGE.md` is outside this task's scope and was not edited; this
  work is fully SI-generated (Devin), per commit trailer.

## Verify commands and results

- Repro (before fix, scratch test): `TypeError: Cannot read properties of
  null (reading 'clearRect')` at uPlot.cjs.js:4888 `_commit`, "Errors 1
  error", exit 1.
- Regression test fails without the fix: ran with `setupFiles: []` →
  `expected [TypeError ... 'clearRect'] to deeply equal []`, 1 failed.
  Reverted immediately.
- `npx tsc --noEmit` → clean.
- `npx vitest run` ×10 loop: `run N exit=0 Tests 271 passed (271)
  errors=0` for all 10 runs (grep for Unhandled/Uncaught = 0). Count
  went 270 → 271 (new regression test).
- `npx eslint .` → clean.
- `npx vite build` → OK (85.93 kB gz main chunk).
- `npx playwright test` → 66 passed, 2 skipped (@live). `test-results/`
  and `playwright-report/` absent (artifacts configured under
  `dist/test-results/`, `dist/` gitignored).

## Open issues

- The no-op ctx means the Canvas2DRenderer now "draws" in jsdom tests;
  pixel-level assertions still need Playwright (unchanged situation).
- uPlot `ctx` typing (`uplot.ts` `UPlotInstance.ctx`) is unaffected — no
  source change made.
