# Handoff: p2-frontend-perf

Status: DONE. All gates green; measurable gains kept, no backend/shared changes.

## What was built

### 1. Bundle (code splitting + vendor chunks + sourcemaps)
- `vite.config.ts`: `sourcemap: false` for the production build (was `true`,
  shipped ~1.1 MB of `.map` files); `manualChunks` splits `vendor-react`
  (react/react-dom/scheduler) and `vendor-zod` so app deploys only invalidate
  the small `index` chunk.
- `App.tsx`: `SpectrumPanel` and `AboutDialog` now load via `React.lazy` +
  `Suspense` (they were statically in the main chunk; `AladinOverlay` and
  `uPlot` were already lazy and stay so).
- Fonts: verified already optimal — latin-only woff2, only the four weights
  actually used (Sans 400/600, Mono 400/500, all referenced by tokens.css).
  No change needed.
- `index.html`: `preconnect` + `dns-prefetch` for `aladin.cds.unistra.fr` and
  `cdn.jsdelivr.net` (the on-demand Aladin Lite sources).

### 2. Decode path (base64 -> Float32 -> texture)
- `lib/base64.ts`: decoder now validates with a strict regex and delegates to
  native `atob` (JS per-quad loop kept as fallback). Measured on a
  192x192 float32 frame (196 kB base64): 2.5 ms -> 0.5 ms per frame
  (~5x; 59 ms -> 11 ms for 24 frames, Node 20).
- `hooks/useFrames.ts`: real fetches decode in `decodeProgressive` — one frame
  at a time, emitting each partial result so the first cutout paints while
  the rest decode, yielding to the event loop whenever a slice exceeds 6 ms.
  The final emit is still the only one that settles polling flags. Mock path
  keeps synchronous decode (tiny payloads).
- `lib/gl.ts`: `upload()` reuses the R32F texture via `texSubImage2D` when the
  frame size is unchanged (was `texImage2D` reallocating every frame);
  `Canvas2DRenderer` reuses `ImageData` the same way; uniform locations are
  looked up once at init instead of per draw.

### 3. Playback (no React render per animation frame)
- `TimeLapseCanvas.tsx`: the rAF loop now advances a live index in a ref and
  paints imperatively — canvas upload/draw + direct DOM writes to the aria
  label, sr status, MJD/detector/flag/blink chips, frame counter and scrubber
  (the flag chip is now always rendered and toggled via `hidden`). The live
  index commits to `transport.frameIndex`/the URL exactly once, on pause or
  natural stop. Scrub/step/keyboard during play steer the live index without
  URL writes. Measured: `history.replaceState` calls during 1.5 s of 30 fps
  playback went 47 -> 2 (play start + pause commit).
- `prefers-reduced-motion`: the A/B blink swap rate is capped at ~2 Hz under
  `reduce` (WCAG flash threshold ~3/s); CSS-level rules were already in place.

### 4. Perceived speed
- Progressive decode (above) shows the first frame as soon as it decodes —
  with a 24x192x192 intercepted real-path payload the first frame painted at
  ~174 ms and the full series at ~198 ms after navigation, with zero >50 ms
  long tasks.
- `lib/api.ts` `prefetchFrames()`: deduplicated low-priority GET of the exact
  frames URL, wired to hover/focus of each `TargetMenu` item via new
  `onPrefetch`/`onPrefetchTarget` props (AppShell -> TargetMenu -> App).
  Skipped in mock mode. CLS unchanged at 0 (no layout changes).

### 5. Measurement tooling
- `tests/perf/measure.mjs`: Playwright script (node, vs `vite preview`)
  reporting nav->first-frame, nav->all-frames, `history.replaceState` count
  during playback and long tasks. `PERF_INTERCEPT=1` serves a generated
  contract-valid payload (PERF_COUNT/PERF_W/PERF_H) through route
  interception to exercise the real fetch/decode path.
- `App.tsx` gains `?mock_w/?mock_h/?mock_n` to resize the synthetic series
  (same pattern as the existing `?mock_delay`).

## Bundle sizes (npm run build, gzip)

| Asset | Before | After |
|---|---|---|
| index.js | 277.16 kB (86.35 gz) + 849 kB map | 65.77 kB (23.35 gz), no map |
| vendor-react | — (inside index) | 141.67 kB (45.36 gz) |
| vendor-zod | — (inside index) | 57.04 kB (13.76 gz) |
| uPlot chunk | 52.41 kB (23.20 gz) + 285 kB map | 52.57 kB (23.23 gz), no map |
| SpectrumPanel | — (inside index) | 10.13 kB (4.34 gz) |
| AboutDialog | — (inside index) | 6.56 kB (2.49 gz) |
| AladinOverlay | 4.35 kB (2.10 gz) | 4.31 kB (2.07 gz) |
| index.css | 33.95 kB (6.60 gz) | 30.54 kB (6.17 gz) |
| First-load JS gzip | 86.35 kB | 82.4 kB (index+react+zod) |
| Sourcemaps shipped | ~1.15 MB | 0 |

## Runtime metrics (Playwright/Chromium vs vite preview, this machine)

| Metric | Before | After |
|---|---|---|
| history.replaceState during 1.5 s play @30fps | 47 | 2 |
| Decode 24x192x192 (Node bench) | ~59 ms single block | ~11 ms, sliced <=6 ms, first frame after ~0.5 ms |
| Long tasks >50 ms on load+decode (real path, 24x192x192) | — | 0 (before path decoded all frames in one sync block) |
| First frame paint, real-path 24x192x192 intercept | — | ~174 ms after nav (progressive; before it painted only after the whole series decoded) |

## Files changed
- frontend/vite.config.ts, frontend/index.html
- frontend/src/App.tsx (lazy panel/about, prefetch wiring, mock dims)
- frontend/src/lib/base64.ts, api.ts, gl.ts
- frontend/src/hooks/useFrames.ts
- frontend/src/components/viewer/TimeLapseCanvas.tsx
- frontend/src/components/layout/{AppShell,TargetMenu}.tsx
- frontend/tests/perf/measure.mjs (new)

## Decisions / deviations
- No Web Worker for decode: with `atob` the whole 24-frame job is ~11 ms;
  the cooperative slice keeps every main-thread task <6 ms and avoids
  worker/transfer plumbing. Chosen per task's "decode lazily OR in a
  worker" alternative.
- No React.memo added: with imperative playback there are no per-frame
  renders left to memo away; the remaining renders (poll/decode emits) are
  already cheap. Task says keep only measurable gains.
- SkyExplorer.tsx (unused standalone composition) untouched.
- `FrameMetadata` panel still tracks the committed (paused) index; during
  playback it updates on pause. The canvas overlay chips do track the live
  frame imperatively.
- SI_USAGE.md is outside this task's scope; note for PM: the whole diff
  above was written by Devin (SI-assisted).

## Open issues
- zod (13.76 gz) still loads eagerly at boot (api.ts + urlState need it).
- First-frame paint could go lower by streaming/decode-as-you-parse the
  JSON payload — out of scope for a no-backend-change task.

## Verify
- `cd frontend && npx tsc --noEmit` — clean
- `npx eslint .` — clean
- `npx vitest run` — 331/331 pass
- `npx playwright test` — 66 pass / 2 @live skipped
- `node tests/perf/measure.mjs` (vite preview on :4173) — metrics above
- react-doctor `--scope changed` — no issues
