# Handoff: f9-real-data-integration

Status: **done**. All gates green. Real-backend end-to-end behaviour
(cold-start latency, partial answers filling in, genuine 502s) still
needs the separate Chrome QA run — every new state is covered by routed
or mocked e2e, but nothing here has talked to the real `/api/v1/frames`
yet.

## Files

- `frontend/src/lib/poll.ts` — **new.** Pure polling policy:
  `expectedFrames(mode, maxFrames, total)` → `min(24, total)` for raw,
  `null` for combining modes; `nextPollDelay(input)` → `POLL_DELAY_MS`
  6 s while a series can still grow, exponential backoff from
  `RETRY_DELAY_MS` 4 s (×1.8, cap 20 s, ≤ 4 retries) only for
  `upstream_unavailable`, `null` otherwise. `POLL_BUDGET_MS` 90 s total
  cap, `MAX_STALE_POLLS` 2 identical answers before stopping.
- `frontend/src/hooks/useFrames.ts` — rewritten around
  `startFramesPoll(apiBase, params, alive, emit)` (module-level, returns
  a stop function): one request in flight, `AbortController` +
  `seq`-guard on unmount/param change, `clearTimeout` on teardown.
  Result gains `errorCode`, `pending` (a follow-up is scheduled),
  `expected` (target count) and `partial` (polling gave up below the
  target). Mock path simulates the contract: a partial answer
  (`truncated: true`, first third) with `pending: true`, the full
  series `mockDelay` ms later (`?mock_delay=<ms>` overrides the 800 ms
  default for e2e).
- `frontend/src/lib/urlState.ts` — `det` key (`1`-`6`, integer; any
  other value ignored → `null` = all detectors). Parsed, serialized,
  round-trips.
- `frontend/src/lib/types.ts` — `MODE_LABELS` moved here from
  ModeSelector (non-component export rule).
- `frontend/src/components/controls/ModeSelector.tsx` — imports
  `MODE_LABELS`; detector `onChange` already emitted
  `{detector}` — now wired to the URL.
- `frontend/src/App.tsx` — detector state moved from local
  `useState` to `url.det`; `maxFrames: 60` **dropped** from the frames
  params (contract default 24 is the pre-warmed selection); `mockDelay`
  plumbing; new state branches (texts below).
- `frontend/src/components/viewer/SkyExplorer.tsx` — same `url.det` +
  no-`max_frames` changes, plus a "Loading more frames… n so far" status
  line while `pending`.
- `frontend/src/components/spectrum/pick.ts` — linear RA/Dec mapping
  replaced by the TAN (gnomonic) inverse from the integration patch:
  `(u,v)` → `(xi,eta)` via CD, deprojected about `crval`. Fixes the
  missing `1/cos(dec)` factor (2.5× RA error at dec +66.75).
- `frontend/src/components/spectrum/useSpectrum.ts` — after a successful
  answer, if it came back in < 15 s with only 1–2 points the request is
  re-issued **once** after `SPECTRUM_RETRY_MS` 5 s (the backend completes
  in the background). The retry is "quiet": it replaces data in place,
  never flips back to loading, and a failed retry keeps the partial
  spectrum on screen. Abort/timer cleanup on close/re-pick/unmount.
- `frontend/src/lib/mock.ts` — mock WCS now `cd: [[-s,0],[0,-s]]`
  (`CD2_2 < 0`, row 0 = north) to match what the backend delivers.
- `frontend/src/lib/presets.ts` — epoch-2026.0 coordinates from
  b11-presets-epoch (table below); ids/labels unchanged; notes now carry
  the epoch and WISE 0855's AllWISE provenance.
- `frontend/src/components/about/AboutDialog.tsx` — new "Targets"
  paragraph: presets are epoch-2026.0; WISE 0855's challenge catalogue
  position (08h55m10.831s, −07d14m42.53s) is the AllWISE epoch-2010.5
  position. No logos added.
- `frontend/src/components/layout/app-shell.css` — `.app-status-retry`:
  inline text-button for "Partial: … Retry" (44 px hit area, accent
  colour, tokens only).
- `frontend/tests/` — `viewer/poll.test.ts`, `viewer/presets.test.ts`,
  `viewer/orientation.test.ts`, `integration/useFrames-poll.test.tsx`,
  `spectrum/useSpectrum.test.tsx` (new); `spectrum/pick.test.ts`,
  `viewer/urlState.test.ts` (updated); e2e `polling.e2e.ts` (new),
  `presets.e2e.ts`, `url-state.e2e.ts`, `live.e2e.ts` (updated).

## What was built

**Polling contract.** `useFrames` re-requests the identical URL while
the raw-mode series is below `min(24, total_observations)` — *not* while
`truncated` is true, since the backend keeps `truncated` set when the
selection is capped at 24. It also stops after 2 consecutive answers
that add nothing, or at the ~90 s budget. A 502 `upstream_unavailable`
retries with backoff (≤ 4); any other error (400/404/etc.) is final and
never retried. Parameter changes or unmount abort in-flight and pending
requests via `AbortController` + a sequence guard.

**Honest progress UI.** First decoded frames are playable immediately —
the polling loop only appends. Exact texts:

- Loading, nothing yet: `Loading frames…` inside the skeleton's
  `role="status"` line; while a 502 retry is pending:
  `The NASA archive is slow to answer — retrying…`
- Partial series still growing: `Loading frames n / N` (raw, expected
  known) or `Loading more frames… n so far` (combining modes).
- Polling ended below the target: `Partial: n of N frames. Retry`
  (inline accent button, remounts the fetch effect).
- Series capped at 24 by the backend (not an error):
  `Showing 24 frames spread over T observations.`
- Terminal 502: `The NASA archive is not answering right now.` inside
  the existing `ErrorState` with its **Retry** button (`role="alert"`).
- 404 / empty raw result: `No SPHEREx observations cover this position
  yet` + hint `Try a wider field of view or pick a target.` (`EmptyState`).
- Diff/Co-add/Stack with 0 frames:
  `Not enough comparable frames at this position` + hint
  `{Mode} needs several exposures of the same detector and band. Try
  Raw or another field.`

**Spectrum click.** `pixelToSky` now applies the gnomonic inverse, so
clicks are correct at high declination (verified by a regression test
near dec +66.75). Sparse fast answers (1–2 points) trigger one silent
re-request at 5 s; the panel already shows `Loading spectrum…` from f5.

**Detector in the URL.** `det` joins `mode`, `stretch`, `cmap`, `fps`,
`frame`, `embed`, `mock`, `hint`. Changing it via the radiogroup resets
`frame` to 0 and re-requests `/frames` with `detector=N`.

**Presets (b11, epoch 2026.0).**

| Target          | RA            | Dec            | fov  |
|-----------------|---------------|----------------|------|
| Barnard's Star  | 17h57m47.102s | +04d46m05.97s  | 0.1  |
| Epsilon Indi B  | 22h04m23.201s | −56d48m02.34s  | 0.1  |
| WISE 0855       | 08h55m02.344s | −07d14m31.86s  | 0.1  |
| RW Aurigae      | 05h07m49.581s | +30d24m04.65s  | 0.05 |
| R Draconis      | 16h32m40.136s | +66d45m18.09s  | 0.05 |

**Orientation.** Backend frames are north-up (`CD2_2 < 0`, row 0 =
north/top); the renderer already draws row 0 at the top (WebGL v_uv flip
and Canvas2D row-major write both do), and the mock WCS now matches.
`orientation.test.ts` pins this with an asymmetric synthetic frame.

## Decisions / deviations

- **Applied by hand, not `git apply -3`.** The integration patch
  predates f6/f8; every hunk was ported into the current layout
  (`Explorer` inside `App.tsx`, `SkyExplorer` standalone view). Look and
  structure preserved.
- **`max_frames` removed entirely** (was `60` in both App and
  SkyExplorer): the contract default 24 is the pre-warmed selection.
- **Poll interval 6 s / budget 90 s** — matches the patch's constants
  and the backend's observed fill-in cadence.
- **`stale polls` stop condition** added beyond the patch: a series that
  twice answers with no new frames ends in the *Partial* state instead
  of polling forever, which is what makes the "Partial … Retry" UI
  reachable in practice.
- **`?mock_delay=`** (e2e/dev only) stretches the simulated partial
  phase; the mock always splits a multi-frame series (first third, then
  all) so pending/partial states are reachable offline.
- **`react-doctor` note:** the detector's `effect-needs-cleanup` flagged
  the timer living in a nested `schedule` closure. Instead of accepting
  the known false positive, the poll loop was hoisted into module-level
  `startFramesPoll` returning a cancel function — same cleanup, no
  timer inside `useEffect`. Remaining warnings: `Explorer`/`SkyExplorer`
  cyclomatic complexity (pre-existing structure, warning level) —
  refactoring them is out of scope.
- 404-empty and mode-specific-empty reuse `EmptyState`; 502 keeps the
  existing `ErrorState`+Retry. All status lines use `role="status"` or
  `role="alert"` as before, tokens only.

## Open issues / for the QA task (real backend + Chrome)

- First-frame cadence on a cold field: the backend's soft deadline and
  how many frames arrive in answer #1 is unverified. `Loading frames n
  / N` assumes `total_observations` arrives in the first answer.
- The 5 s sparse-spectrum retry heuristic (< 15 s, 1–2 pts) is a guess
  at the b14 completion latency; confirm on real data.
- `stale=2` might end polling early if the backend serves two identical
  partials in a row under heavy load — watch real traces.
- Live e2e (`E2E_LIVE=1`, `tests/e2e/live.e2e.ts`) updated to the
  epoch-2026.0 positions — run it against the deployed backend.

## Verify

```
cd frontend
npx tsc --noEmit          # clean
npx vitest run            # 18 files, 227 tests passed
npx eslint .              # clean
npx vite build            # ok (index 271 kB / 84 kB gzip)
npx playwright test       # 62 passed, 2 skipped (@live, opt-in)
npx react-doctor@latest --verbose --scope changed
                          # 74/100 — 2 complexity warnings only
                          # (pre-existing, no regressions)
```
