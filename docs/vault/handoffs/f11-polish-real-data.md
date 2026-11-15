# Handoff: f11-polish-real-data

Status: **done**. Frontend-only polish of the issues found by the
f10 real-data QA. Every change has a regression test written first;
all gates are green. Branch `devin/f11-polish-real-data` (parent:
`opus/f10-real-data-qa`).

## What was built

### 1. Calm end state for archive-short series (polling)

`poll.ts` now exposes `stopReason()` alongside `nextPollDelay()`,
classifying every polling stop as `"complete"`, `"stale"` (the answers
stopped growing below the target — the archive has nothing more to
give), `"budget"` (the 90 s window ran out mid-growth) or `"error"`.
`useFrames` maps the result to two UI states:

- `settled` (was previously lumped into `partial`): a stale stop below
  the expected count. `App.tsx` renders the calm neutral note
  `"N of M frames available (the archive did not provide the rest)"`
  with a quiet secondary `Check again` button — no warning colour, no
  prominent Retry. This is the WISE 0855 end state: its series ends at
  22 of 24 because two upstream cutouts always return 422.
- `partial` (unchanged visuals): reserved for stops on the time budget
  or terminal errors, where a retry can still add frames.

For Diff, Co-add and Stack the expected count is unknowable, so a
shorter follow-up cadence (`POLL_DELAY_COMBINING_MS = 4000`) plus the
existing `MAX_STALE_POLLS = 2` settles a stopped series in ~8 s instead
of showing `Loading more frames…` while re-downloading identical
answers (raw mode keeps the 6 s cadence, which matches the backend's
background-fill speed).

### 2. Honest WISE 0855 / Barnard's Star copy

- `presets.ts` note (WISE 0855): very faint, extremely cold brown dwarf;
  best in long-wavelength detectors (D5, D6) with Co-add or the ZScale
  stretch; ~8"/yr (~1.3 native pixels) is near the limit of what
  SPHEREx pixels show. `ra`/`dec`/`fov` of all five presets are
  byte-identical (baked cache keys preserved; `presets.test.ts` pins
  them).
- About → Targets paragraph says the same, and that Barnard's Star
  drifts about 10 arcsec per year to the north.

### 3. Spectrum aperture default 5 px

`useSpectrum` now requests `aperture_px=5` by default (was 3): on
Barnard's Star the 3 px aperture missed the star between epochs and
the measured values jumped 165→1085 MJy/sr. Options remain 1/3/5;
the select carries `title="Aperture radius in pixels — a larger
aperture follows moving stars better"`.

### 4. Robust spectrum Y axis

- `prepare.ts` exports `robustYRange(points)`: finite values only,
  unflagged points preferred (falls back to all finite points when
  everything is flagged), interpolated 2nd–98th percentile + 10 %
  span margin, nonzero fallback padding for degenerate spans (single
  point, all-equal, all-zero). Returns `null` when no finite value
  exists. `PreparedSpectrum` now also carries per-point sorted
  `flags` and `detectors` arrays.
- `SpectrumPanel.tsx` clamps the uPlot y range to the robust limits
  (the flagged 103.8 MJy/sr point on Barnard's Star used to squash the
  ~0.3 MJy/sr continuum flat), keeps error bars (clamped to the plot
  bounds), draws flagged points as hollow rings vs filled dots for
  clean points, draws triangle markers at the top/bottom edge for
  values outside the range, and shows a noninteractive hover tooltip
  (wavelength, value, sigma, detector, flag names, "off scale" note).
- `uplot.ts` local typings gained `setCursor` hooks, `cursor` position
  fields and `scales.y.range`; `spectrum.css` gained the `.sp-tooltip`
  style (hidden by default, pointer-events none).

### 5. REFERENCES.md

The Aladin Lite entry now documents the jsDelivr/npm fallback
(`aladin-lite@3.8.2/dist/aladin.js`) and notes that the CDS `aladin.js`
URL returned HTTP 404 at the time of writing (2026-11-15).

## Files changed

Source: `frontend/src/lib/poll.ts`, `frontend/src/hooks/useFrames.ts`,
`frontend/src/App.tsx`, `frontend/src/lib/presets.ts`,
`frontend/src/components/about/AboutDialog.tsx`,
`frontend/src/components/spectrum/{useSpectrum,prepare,SpectrumPanel,uplot}.ts(x)`,
`frontend/src/components/spectrum/spectrum.css`, `docs/REFERENCES.md`.

Tests: `frontend/tests/viewer/{poll,presets}.test.ts`,
`frontend/tests/integration/useFrames-poll.test.tsx`,
`frontend/tests/spectrum/{prepare,useSpectrum}.test.{ts,tsx}`,
`frontend/tests/e2e/{polling,spectrum}.e2e.ts`.

## Verify commands and results

All run from `frontend/` on Node v20.20.2 (npm warns about its own
v12 requiring Node ≥22 — noise only):

| Gate | Result |
|---|---|
| `npx tsc --noEmit` | clean |
| `npx vitest run` | 21 files / 270 tests pass |
| `npx eslint .` | clean |
| `npx vite build` | ok (index 276 kB / 86 kB gzip) |
| `npx playwright test` | 66 pass, 2 skipped (`@live`, opt-in) |

The full Playwright run includes the a11y/axe suite
(`tests/e2e/a11y.e2e.ts`) and the responsive suite (no horizontal
overflow at 375/768/1440 px, interactive targets ≥ 44 px) — all green
for the changed states. The new spectrum e2e drives a routed
`/api/v1/spectrum` answer with a flagged 103.8 MJy/sr outlier and
asserts the `aperture_px=5` request, the flag names and the
"off scale" note in the hover tooltip. `test-results/` and
`playwright-report/` were deleted after the run.

## Decisions and deviations

- **Stop-reason API shape.** `stopReason()` reuses `nextPollDelay()`'s
  inputs and mirrors its checks in the same order, so the two can
  never disagree on whether a follow-up is due. The "budget" reason
  covers an `upstream_unavailable` stop only when the retry budget is
  exhausted, because a plain budget overrun with frames growing is
  the retryable case the warning is meant for.
- **8 s combining-mode bound.** Interpreted "about 8 s / two
  follow-ups" literally: two unchanged answers 4 s apart (first stale
  at t+4 s, second at t+8 s) rather than two extra requests at the
  raw cadence. The e2e "diff mode with zero comparable frames" now
  settles in ~8.5 s (was ~12 s+ before).
- **`settled` vs `partial` are mutually exclusive** in `useFrames`;
  a terminal error after a partial answer always ends in `partial`
  (warning), never the calm note — a retry may still add frames.
- **`SI_USAGE.md` not edited**: it sits outside the allowed scope
  (`frontend/src/`, `frontend/tests/`, `docs/REFERENCES.md`,
  `docs/vault/handoffs/`). The orchestrator may want to add the
  standard Devin row for this task; everything here is SI-generated.

## Open issues

- None blocking. The two permanently-422 cutouts behind WISE 0855's
  22-of-24 series are upstream (IRSA) behaviour, not fixable here; the
  UI now explains it honestly instead of warning.
- The hover tooltip is intentionally noninteractive and pointer-only;
  keyboard users get the same numbers through the panel's footer text
  and the point-count summary, not per-point. Acceptable for the demo
  scope but worth revisiting if full keyboard chart access is ever
  required.
