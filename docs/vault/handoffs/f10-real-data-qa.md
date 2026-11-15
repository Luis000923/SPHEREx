# Handoff: f10-real-data-qa

Status: **done**. I ran end-to-end QA of the real product in Chrome against the final release backend (`b17-bake-verify/backend/target/release/spherex-backend`) and a copy of the baked cache (`cache-bake`, 455 files). Seven frontend defects were found and fixed, each with a regression test written first. All gates are green. The open items below need decisions from the PM or the backend owner.

Branch `opus/f10-real-data-qa`. Commits (each ends with `Generated-by: Claude Opus 5.5 (SI-assisted)`):

| Commit | Fix |
|---|---|
| `028662b` | Exact preset strings in the header menu, letterbox-aware spectrum click, Stack polling stops after its one image |
| `ff15dd3` | Playback keeps the requested fps |
| `2cc510a` | Diff frames use a display interval centred on zero |
| `936afd0` | Aladin Lite loads from jsDelivr when the CDS URL fails |
| `ecf25b6` | The embed credit link has a 44 px hit area |
| `3110bdf` | A calm message replaces "Failed to fetch" when the server is down |

## Setup used

- Backend: `CACHE_DIR=<scratch copy of cache-bake>` (all files touched first), `STATIC_DIR=frontend/dist`, `PORT=18096`, `WARM_TARGETS=0`.
- Offline mode: `IRSA_BASE_URL`, `SPHEREX_S3_BASE_URL` and `IRSA_TAP_URL` all set to `http://127.0.0.1:9`.
- Online mode: the same server without those variables.
- Chrome was driven through the Chrome MCP tools in my own tab.
- Numeric pixel analysis was done in Python on the exact `/api/v1/frames` JSON the browser received.
- Playwright headless Chromium was used for the frame-rate and memory numbers, because Chrome MCP throttles `requestAnimationFrame` to about 6.5 Hz even on an idle page.

## Per-preset results (offline, baked cache, opened by URL with the exact `presets.ts` strings)

| Preset | Raw frames | Diff / Co-add / Stack | First frame after navigation | Target visible | Centre spectrum (points, first / nearby click) | Field-star spectrum |
|---|---|---|---|---|---|---|
| Barnard's Star | 24 of 390 obs | 13 / 10 / 1 | 741 ms (982 ms in Chrome) | **Yes, very bright.** Peak 6e3–3e4 σ. Display-clipped at the top of the linear 1–99 range. Centre NaN in 3 of 24 frames. | 23 points, 1.9 s / 2 ms | 23 points, 2.1 s (has a flagged 103.8 MJy/sr outlier at 4.93 µm) |
| Epsilon Indi B | 24 of 488 | 15 / 9 / 1 | 685 ms | **Yes.** Peak 18–870 σ. Above 5σ in 22 of 24 frames; in 2 frames the centre is off the footprint. | 22 points, 3.4 s / 2 ms | 22 points, **40 s** (unbaked position offline: the backend waits out its deadline) |
| WISE 0855 | **22 of 280** (Partial) | 11–14 / 8 / 1 | 651 ms (first response 3.9 s in Chrome after a backend restart) | **No.** The centre peak is not distinct: 1–4 % of the pixels in the frame are brighter. It is only 3–7 σ at 3.1–4.6 µm. No measurable motion. | 21 points, 2.7 s / 3 ms. These points are field and background, not the brown dwarf. | 21 points, 4.5 s |
| RW Aurigae | 24 of 341 | 18 / 6 / 1 | 292 ms | **Yes, bright.** 150–11 700 σ, core not NaN. | 24 points, < 0.1 s | 24 points, < 0.1 s |
| R Draconis | 24 of 565 | 14 / 9 / 1 | 294 ms | **Yes, saturated.** The centre is NaN in 12 of 24 frames (all D4 frames, most D2 and D3). The core shows as a black hole. | 12 → 13 points, 5.8 s / 0.3 s | not completed (script time limit) |

Other checks:

- **Modes.** Every mode showed a non-blank canvas, and the Stack mode response was always 1 frame.
- **Network requests.** The browser sent no failing request in offline mode for any preset, mode or centre spectrum.
- **Header menu.** After the fix, every preset chosen from the header menu loads from the cache. Before the fix every one was a miss; see D1 below.
- **Comparison with b17 `live-results`.** Frame and spectrum counts match. The one difference is WISE 0855 Diff: 11 frames by curl, 14 in the browser after polling.

### Orientation (real data)

North is up and east is left. I measured the centroid drift with a linear fit over each raw series (pixel scale 1.875"/px):

- **Barnard's Star** moves up (north) at 8.9"/yr against 10.4 expected. RA drift is −2.7"/yr against −0.8 expected.
- **Epsilon Indi B** moves left (east) at 3.3"/yr against 4.0 expected, and down at −2.7"/yr against −2.5 expected.
- **Diff on Barnard's Star** (2026-08-21, frame 7 of 13) shows a bright lobe above and a dark lobe below the star: `docs/screens-real/barnards-star-1440-diff-after-fix-north-up.jpg`.

### Stretch and colormap defaults

I rendered every preset with all four stretches (linear, log, asinh, zscale) from the real frames.

- **Raw:** the default linear 1–99 stretch shows stars, not the zodiacal gradient, on all five fields. Log brings out the gradient and NaN masks. ZScale is good for faint fields such as WISE 0855. I kept the default unchanged.
- **Diff:** this one was wrong; see D4.

## Defects

### Fixed (frontend)

**D1. Header menu missed the baked cache.**
- *Problem:* `App.tsx` re-formatted the degrees from `/api/v1/targets` with 2 decimals for RA and 1 for Dec. The request became `GET /api/v1/frames?ra=17h57m47.10s&dec=%2B04d46m06.0s&fov=0.1&mode=raw`, which returned **502 offline**.
- *Fix:* `menuTargetsFromServer()` in `lib/presets.ts` keeps the `presets.ts` strings byte-identical for known ids. Unknown ids get 3 and 2 decimals.
- *Test:* `tests/viewer/presets.test.ts`.

**D2. Spectrum click ignored letterboxing.**
- *Problem:* the canvas uses `object-fit: contain`, so a square cutout in a wide stage has bars on the sides. The click mapping used the whole 1118×823 element rectangle. A click 54 px right of centre landed about 14 px (26") off.
- *Fix:* `clientPointToPixel()` now accounts for the bars, and clicks on the bars are ignored. Verified on real data: the requested RA is 269.41761 against 269.41777 expected.
- *Test:* `tests/spectrum/pick.test.ts`.

**D3. Stack kept polling after its single image arrived.**
- *Problem:* it showed "Loading more frames… 1 so far" for about 13 s and re-downloaded the same answer twice.
- *Fix:* `nextPollDelay()` stops for Stack once at least 1 frame is in. Stack mode now settles in 150–450 ms.
- *Test:* `tests/viewer/poll.test.ts`.

**D4. Diff display interval was not centred on zero with the grayscale colormap.**
- *Problem:* "no change" sat at 0.80 of the display range on Barnard's Star, was clipped to white on R Draconis, and moved from frame to frame.
- *Fix:* `frameInterval()` in `lib/stretch.ts` gives signed frames a symmetric interval. Diverging colormaps also get it, and an explicit ZScale choice is kept. `TimeLapseCanvas` has a new `signed` prop, set when mode is `diff`.
- *Test:* `tests/viewer/stretch.test.ts`.
- *Screenshots:* `*-diff-before-fix.jpg` and `*-diff-after-fix*.jpg`.

**D5. Playback was slower than the fps setting.**
- *Problem:* 30 fps played at about 20 fps (101 steps in 5 s at 60 Hz), and 5 fps at about 4.6 fps. The rAF effect restarted on every frame and set its accumulator to 0.
- *Fix:* `lib/playClock.ts` carries the leftover time between steps, and the loop reads frame state through refs. Measured after the fix: 150 steps in 5 s at 30 fps, and 25 at 5 fps.
- *Test:* `tests/viewer/playClock.test.ts`.

**D6. The Aladin overlay always failed.**
- *Problem:* CDS answered **404** for `https://aladin.cds.unistra.fr/AladinLite/api/v3/3.8.2/aladin.js`, and also for `latest`, 3.6.x and 3.7.x, on 2026-11-15 around 06:00 UTC.
- *Fix:* `loadAladin()` falls back to `https://cdn.jsdelivr.net/npm/aladin-lite@3.8.2/dist/aladin.js`. That file is an ES module whose default export is `A`.
- *Result in Chrome:* the overlay loads. However, the HiPS sky stayed grey at test time, because the CDS tiles were not served.
- *Test:* `tests/spectrum/aladinLoader.test.ts`.

**D7. The embed credit link was 270×15 px.**
- *Fix:* it now has a hit area of `min-height: var(--target)` (44 px).
- *Test:* an assertion added to `tests/e2e/about.e2e.ts`.

**D8. Backend down showed the browser's raw "Failed to fetch".**
- *Problem:* the raw text appeared in both the viewer and the spectrum panel.
- *Fix:* `apiFetch()` in `lib/api.ts` maps the error to `ApiError` code `network` with the message "Cannot reach the SPHEREx Blink server. Check your connection and retry." Aborts pass through unchanged.
- *Test:* `tests/viewer/networkError.test.ts`.
- *Verified:* after a restart, Retry recovers.

### Open (needs a decision or another owner)

**O1. WISE 0855 ends in "Partial: 22 of 24 frames. Retry" on fully baked data** (owner: backend + PM, contract).
- The backend selects 24 frames, but 2 cutouts are permanently skipped (HTTP 422). The frontend cannot tell "still filling" from "done". It polls twice more (2.7 MB each), then shows Partial with a Retry that cannot help.
- The same root cause makes Diff and Co-add show "Loading more frames… n so far" for about 12 s on cached data.
- Proposal: add `complete: bool` (or `pending`) to `FramesResponse`. This is a contract change, so it is the orchestrator's call.

**O2. WISE 0855 is not visible** (owner: PM, messaging).
- It is a Y dwarf, invisible below about 3 µm and marginal at 3.1–4.6 µm in single frames. Neither the target nor its 8"/yr motion is measurable in raw frames, and the centre spectrum is the background.
- Suggestion: say so in the preset note or the About text, for example "faint: look at D5–D6 with ZScale/Co-add".

**O3. Barnard's Star spectrum at the default 3 px aperture is jumpy** (owner: PM / backend).
- Values swing between 165 and 1085 MJy/sr from band to band. The star moves 2–5 px between epochs relative to the fixed epoch-2026.0 position.
- A 5 px aperture gives a smooth 152–382 MJy/sr series.
- Options: a default aperture of 5 px for the movers, or an aperture that follows proper motion in the backend.

**O4. Flagged outliers crush the spectrum y-axis** (owner: PM / design).
- Example: the field-star spectrum near Barnard's Star has one point at 103.8 MJy/sr with flags TRANSIENT, SUR_ERROR and PERSIST, against about 0.3 MJy/sr elsewhere. The flags are shown, but the chart scale is dominated by that point.
- Options: robust y-limits, or drawing flagged points differently. Rule 7 forbids hard-coding flag meanings, so this needs a design decision.

**O5. Backend upstream behaviour** (owner: backend).
- With real network, IRSA SIA returned HTTP 502 and the TAP fallback also 502 (around 06:22 UTC). The backend then answered 502 in **0 ms** to the frontend's 4 retries, so the 502 seems to be negative-cached. The retries cannot succeed inside that window.
- In offline mode, the WISE 0855 raw request takes 3.9 s and Diff/Co-add for WISE 0855 and R Draconis take about 8.3 s, even though the presets are baked. The backend appears to try the dead upstream for the missing pieces first. b17 measured the same 3.7 s for WISE raw.
- An unbaked spectrum in offline mode takes 40 s, the full deadline.

**O6. `docs/REFERENCES.md` should name jsDelivr and npm `aladin-lite` as the Aladin fallback source.** That file is outside my scope.

**O7. Cosmetic issues:**
- At 375 and 768 px, the RA/Dec inputs truncate the 13-character preset strings. The text is scrollable in the field.
- Aladin's own controls are cramped in the 22 rem side panel.
- The spectrum band labels still overlap, as already noted in D-022.

## Responsive, a11y, embed

Measured inside same-origin iframes of the real app:

| View | Horizontal overflow | Canvas | Targets below 44 px |
|---|---|---|---|
| 375×800 | none (scrollWidth = clientWidth) | 349×418 | none |
| 768×1024 | none | 382×670 | only a 16 px checkbox wrapped in its label |
| Embed 375×420 | none | 375×342 | none (the credit link was below 44 px; fixed in D7) |

- The embed hides the header and keeps the credit "SPHEREx Blink — NASA SPHEREx data via IRSA". The credit link opens the full app without `embed`.
- **Light scheme:** I injected the page's `prefers-color-scheme: light` rules (the f6 technique). Computed colours: body `rgb(247,248,250)`, text `rgb(20,23,28)`, Go button `rgb(40,88,184)` on white.
- **Contrast:** the tokens are unchanged since f6, so the f6 contrast table still applies.
- **Spectrum panel:** it never overlapped any control in the five presets at 1440×900.

## Performance

| Measurement | Value |
|---|---|
| First frame, baked, after navigation (headless) | 292–741 ms |
| First frame, baked (Chrome) | 324–982 ms |
| First frame, cold | not measurable: IRSA SIA/TAP were returning 502 |
| Frame rate during playback (headless, 60 Hz) | 300 rAF callbacks in 5 s, average 16.67 ms, p95 16.8 ms |
| JS heap before mode switches | 21.2 MB |
| JS heap after 10 mode switches + GC | 21.6 MB (no growth) |
| Console errors (headless, 5 presets + switches) | 0 |
| Failed responses (headless, 5 presets + switches) | 0 |

In Chrome, the only failing requests were:
- the expected 502s for the unbaked position in offline mode;
- the CDS `aladin.js` 404 (see D6).

## Screenshots (`docs/screens-real/`, 13 JPEGs, 400 kB)

- `barnards-star-1440-raw-spectrum-field-star.jpg`
- `epsilon-indi-b-1440-raw-spectrum.jpg`
- `wise-0855-1440-raw-partial-spectrum.jpg`
- `rw-aurigae-1440-raw-spectrum.jpg`
- `r-draconis-1440-raw-saturated-spectrum.jpg`
- `barnards-star-1440-diff-before-fix.jpg`
- `barnards-star-1440-diff-after-fix-north-up.jpg`
- `r-draconis-1440-diff-after-fix.jpg`
- `barnards-star-1440-overlay-unavailable.jpg` (before D6)
- `barnards-star-1440-overlay-fallback-loaded.jpg`
- `barnards-star-375-768-embed-iframes.jpg` (375 light, 768 dark, embed 375)
- `offline-1440-unbaked-position-502.jpg`
- `rw-aurigae-1440-backend-down.jpg`

## Could not verify

- **Cold real-network loading** (time to first frame, partial answers filling in): IRSA returned 502 during the test window. I sent only 1 cold position and stopped there.
- **"Position without observations":** this needs a working IRSA. In offline mode an unbaked position gives 502, then the terminal error with Retry after about 45 s.
- **Frame rate inside Chrome MCP:** it throttles rAF. I used headless Playwright instead.
- **Real `prefers-color-scheme` emulation in Chrome:** I used injected rules instead.
- **Firefox and Safari:** not tested.
- **The 5 s sparse-spectrum retry (f9 heuristic):** it never fired, because baked spectra arrive complete.

## Verify

```
cd frontend
npm ci
npx tsc --noEmit          # clean
npx vitest run            # 21 files, 250 tests passed
npx eslint .              # clean
npx vite build            # ok (index 272 kB / 84.8 kB gzip)
npx playwright test       # 62 passed, 2 skipped (@live)
```
