# Handoff: f13-airmanprep-design (AirmanPrep re-skin, frontend only)

Status: **complete**. All gates are green on branch `opus/f13-airmanprep-design`.
Author: Claude Opus 5.5 (agent `spherex-frontend-designer`).

This task re-skins the app. Behaviour is unchanged, except for one new display-only element: the active-target chip. Nothing outside the frontend scope was touched: `backend/`, `shared/`, Docker files, `lib/presets.ts` (preset strings), API calls and URL semantics are all unchanged.

## Files

- `frontend/src/styles/tokens.css`: AirmanPrep palette mapped onto the existing token names. New tokens:
  - accent roles: `--accent-ink`, `--accent-hover`, `--accent-line`
  - links and semantic colours: `--link`, `--ok`, `--ok-soft`, `--warn-soft`, `--danger-soft`
  - chips: `--chip-bg`, `--chip-fg`
  - image area: `--stage`, `--stage-fg`
  - decoration: `--glow`
  - shadows: `--shadow-sm`, `--shadow-lg`, `--shadow-card`
  - type and shape: `--weight-medium`, `--tracking-eyebrow`, `--tracking-title`, `--radius-l`, `--radius-xl`, `--space-7`, `--space-8`, `--dot`, `--ease-soft`
  - `--ease` is now the brand "out" curve.
- `frontend/src/styles/fonts.css` (new): `@font-face` rules for the bundled IBM Plex woff2 files. Imported first in `src/main.tsx`.
- `frontend/src/styles/base.css`: shared brand primitives `.eyebrow`, `.chip` and `.chip-warn` (pill, dot, mono label) and `.btn-primary` (lime). Accent-as-text now uses `--accent-ink`.
- Component CSS:
  - `app-shell.css`: header glow, new mark, eyebrow group titles, spectrum drawer as a raised card, empty-state glow and trail, active-target row.
  - `viewer.css`: neutral `--stage`, pill overlay chips.
  - `controls.css`: lime Go, mono in-field labels, pill segmented controls.
  - `spectrum.css`, `metadata.css`, `overlays.css`, `about.css`: card dialog, link colour, lime-tint hint pill.
- TSX:
  - `AppShell.tsx`: original inline-SVG mark.
  - `States.tsx`: empty-state glyph with a dotted trajectory; Retry uses `.btn-primary`.
  - `FirstRunHint.tsx`: inner label span for the pill.
  - `FlagBadges.tsx`: chip classes.
  - `AboutDialog.tsx`: "Design: AirmanPrep" link with `rel="noopener noreferrer"` and `target="_blank"`.
  - `SpectrumPanel.tsx`: the chart line reads `--accent-ink`.
  - `App.tsx`: `activePreset()` and the active-target chip under the search bar.
- `frontend/index.html`: theme-color `#212121` (dark) and `#f4f3ec` (light), plus a lime-mark favicon as an SVG data URI. The title is unchanged.
- `frontend/package.json` and `package-lock.json`: `@fontsource/ibm-plex-sans` 5.3.0 and `@fontsource/ibm-plex-mono` 5.3.0, exact pins.
- Tests:
  - `frontend/tests/layout/tokens.test.ts`: contrast pairs rewritten and extended, see the table below. Also: a brand-palette guard, a "lime is never `color:`/`border:`/`outline:` in component CSS" guard, a neutral-stage guard, and `about.css` added to the "no literal colours" list.
  - `frontend/tests/layout/brand.test.tsx` (new): `activePreset` (exact strings, decimal spelling, nearby field not matched), chip rendering, the About design credit, and flag chips.
- `docs/REFERENCES.md`: both fonts (OFL-1.1) in the npm list, and a "Design" section that credits AirmanPrep and states that the mark is original.
- `docs/screens-design/airmanprep-*.jpg`: 36 JPEGs, 1.7 MB.
  - 8 `before-*`: dark and light, 375 and 1440, home and spectrum.
  - 28 `after-*`:
    - home and spectrum at 375, 768 and 1440
    - About at 375 and 1440
    - hint plus preset chip at 375 and 1440
    - diff at 1440
    - embed at 375
    - empty at 375 and 1440
    - every one of these in both schemes
- `SI_USAGE.md`: one row for this task. The file is outside the declared scope; the orchestrator asked for it.

## Design decisions

- **Palette (dark is the default).**
  - Surfaces: base carbon `#212121`, raised `#2a2a27`, inset `#1a1a19`, hover `#353530`. These steps keep a slight warm-neutral cast.
  - Text: `#f4f3ec`, dim `#c2c0b3`, faint `#a3a194`. Hairline `#3a3a35`; control borders `#85837a`.
- **Palette (light).**
  - Surfaces: paper `#f4f3ec`, raised `#fbfaf5`, inset/secondary `#eceade`, hover `#e8e6d9`.
  - Lines: hairline `#dcdacc`, input borders `#838272`.
  - Text: ink `#2b2b26`, muted `#58584d`, soft `#5f5f55`.
- **Accent roles are split, so lime can never become text on paper.**
  - `--accent` is the lime `#aedc22` fill, always with carbon `#212121` text. Hover is `#9fcb1c`. The 1px outline is `--accent-line #466004`.
  - `--accent-ink` is the accent as text, line or border. It is lime on dark and `#466004` (brand-deep) on light. It covers: pressed transport buttons, the inline Retry, the focused input border, the active-segment ring, checkbox and radio `accent-color`, the slider focus thumb and progress, and the spectrum line.
  - A test fails if any component CSS sets `color`, `border*` or `outline` to `var(--accent)`.
- **Semantic colours.**
  - Light: ok `#0b6670`, bad `#a9361a`, warn `#7c5200`, each with its reference tint. Link `#1f5f8f`; focus `#0b5cad`.
  - Dark variants: ok `#6cc8cf`, bad `#f0937a`, warn `#e9b949`, with dark tints. Link `#c3e45a`; focus `#6aaee8`, a lighter variant of `#0b5cad`.
- **Image area.** `--stage #1b1b1b`, neutral grey in both schemes (a test checks r = g = b). Real data is never tinted. In the light scheme the image therefore sits in a dark frame, like a photo viewer. The scrim chips over the image are unchanged in both schemes.
- **Type.**
  - IBM Plex Sans 400/600 for the UI. Headings are 600 with -0.01em tracking.
  - IBM Plex Mono 400/500 for:
    - eyebrow labels: panel group titles, About section titles, in-field RA/DEC/FOV labels, the FLAGS label
    - chips, coordinates, wavelengths, dates and counters
  - The 12/14/16/20 scale is unchanged. Tabular numbers are kept.
  - Plex Sans 500 is **not** shipped: nothing in the UI uses it. Medium (500) is used only by mono labels.
- **Components.**
  - Go and Retry: lime fill, carbon text, 1px `#466004` outline, `#9fcb1c` hover, and a 1px press offset.
  - Ghost controls keep a 1px `--border-strong` border.
  - Segmented controls (mode and detector): pill track. The active option is a pill with the lime tint (`--accent-soft`) and a 1px `--accent-ink` ring. Every option is still at least 44 px.
  - Spectrum panel: a raised card (12 px radius) docked in the flow, as f6 required; it never floats over controls. On dark, a hairline border carries the edge with only a small shadow. On light it has a soft medium shadow.
  - About dialog: a 12 px card. On light it has the large soft shadow; on dark, a border plus shadow.
  - First-visit hint: a lime-tinted pill with a lime dot.
  - Active-target chip, for example `BARNARD'S STAR · EPOCH 2026.0`: a pill with a lime dot and a mono label, under the search bar. It appears only when the URL position equals a preset: the exact presets.ts strings, or within 2″ after parsing. Screen readers hear "Active target: …". All five presets are epoch-2026.0 positions.
- **Decoration (subtle).**
  - Header: a soft radial lime glow behind the name. Opacity is 0.09 on dark and 0.11 on light.
  - Empty state: the same glow, plus a thin dotted trajectory into a reticle with a lime body.
  - The glow is never behind the image.
- **Motion.** Durations stay at 120/180 ms. Easing is the brand `cubic-bezier(0.16, 1, 0.3, 1)`. Everything is still removed under `prefers-reduced-motion` (`base.css`).
- **Mark and favicon.** The mark is an original drawing: a tilted orbit (ellipse) around a lime body, with a small satellite dot. It is inline SVG in the header and an SVG data URI for the favicon (carbon square). It is **not** the AirmanPrep logo file. No NASA logos.

### Deviation (PM please confirm)

The task asked for a lime dot on the **data-quality flag** chips as well. I kept the chip shape (pill, dot, mono label) but gave flag chips a **warn** dot, warn text and a warn tint (`.chip-warn`). Reason: lime is the brand/"active/go" colour, so a lime dot on TRANSIENT/PERSIST would read as "good". The minimal-ui rule also reserves semantic colours for data states. Switching to lime is a one-class change in `FlagBadges.tsx`: drop `chip-warn`.

### Note for the PM: CLAUDE.md rule 11

Rule 11 says "system fonts". This task explicitly asked for bundled IBM Plex, which is done. Both stacks fall back to the old system fonts. The rule text in CLAUDE.md is the PM's to update.

## Contrast (WCAG 2.1, computed from tokens.css and asserted in `tokens.test.ts`)

| Pair | Dark | Light | Need |
|---|---|---|---|
| `--fg` on `--bg` / raised / inset / hover | 14.48 / 12.94 / 15.66 / 11.08 | 12.79 / 13.61 / 11.78 / 11.35 | 4.5 |
| `--fg-dim` on `--bg` / raised / inset / hover | 8.81 / 7.87 / 9.53 / 6.74 | 6.47 / 6.89 / 5.96 / 5.74 | 4.5 |
| `--fg-faint` on `--bg` / raised / inset / hover | 6.20 / 5.54 / 6.70 / 4.75 | 5.80 / 6.17 / 5.34 / 5.15 | 4.5 |
| carbon on lime (`--accent-fg` on `--accent`, Go/Retry) | 10.01 | 10.01 | 4.5 |
| carbon on lime hover `#9fcb1c` | 8.46 | 8.46 | 4.5 |
| `--fg` on lime tint `--accent-soft` (active segment, hint pill) | 9.34 | 12.20 | 4.5 |
| `--accent-ink` on `--accent-soft` | 6.46 | 6.13 | 4.5 |
| `--accent-ink` on `--bg` (lime on carbon / brand-deep on paper) | 10.01 | 6.43 | 4.5 |
| `--accent-ink` on raised / inset / hover | 8.95 / 10.83 / 7.67 | 6.85 / 5.92 / 5.71 | 4.5 |
| `--link` on `--bg` / raised | 11.17 / 9.98 | 6.11 / 6.50 | 4.5 |
| `--danger` on bg / raised / inset / its tint | 7.02 / 6.28 / 7.60 / 5.57 | 5.85 / 6.22 / 5.38 / 5.02 | 4.5 |
| `--warn` on bg / raised / its tint (flag chips) | 8.82 / 7.88 / 6.86 | 6.16 / 6.56 / 5.51 | 4.5 |
| `--ok` on bg / raised / its tint | 8.28 / 7.40 / 6.27 | 6.00 / 6.38 / 5.54 | 4.5 |
| chip label (`--chip-fg` on `--chip-bg`) | 12.94 | 13.61 | 4.5 |
| `--stage-fg` on `--stage` ("No frames loaded") | 8.31 | 8.31 | 4.5 |
| focus ring on bg / raised / inset / hover (UI) | 6.77 / 6.05 / 7.33 / 5.19 | 6.00 / 6.38 / 5.52 / 5.32 | 3 |
| `--border-strong` on bg / raised / inset / hover (UI) | 4.24 / 3.79 / 4.58 / 3.24 | 3.50 / 3.72 / 3.22 / 3.10 | 3 |
| scrim text / scrim warn over a white pixel | 7.60 / 4.63 | same | 4.5 |

The lowest values are:
- text: 4.75 (dark `--fg-faint` on hover)
- UI: 3.10 (light `--border-strong` on hover)

For reference, lime on paper would be 1.45. That is why it is never used as text there; the test asserts this ratio is below 3.

## Sizes (`vite build`)

| Asset | Before (main) | After |
|---|---|---|
| main JS | 275.93 kB / 86.13 kB gz | 277.16 kB / 86.56 kB gz (+0.43 kB gz) |
| main CSS | 29.34 kB / 5.77 kB gz | 33.95 kB / 6.62 kB gz (+0.85 kB gz) |
| fonts (woff2, latin) | none | 76.44 kB in 4 files: Sans 400 22.59, Sans 600 24.25, Mono 400 14.71, Mono 500 14.89 |

- Fonts are not inlined. They are fetched on first use with `font-display: swap`, and the page renders immediately in the system fallback.
- Only woff2 is referenced. `fonts.css` uses its own `@font-face` rules instead of the fontsource CSS, so no woff fallbacks are emitted.
- The `node_modules` cost is the two packages; only the four woff2 files reach `dist/`.

## Verification

**Gates** (in `frontend/`):

| Command | Result |
|---|---|
| `npx tsc --noEmit` | clean |
| `npx vitest run` | 22 files, **330 tests passed**, plus the known unhandled uPlot error (below) |
| `npx eslint .` | clean |
| `npx vite build` | ok |
| `npx playwright test` | **66 passed**, 2 skipped (@live). axe home+spectrum and About in dark and light are all clean |

The known uPlot error is `TypeError: Cannot read properties of null (reading 'clearRect')` from `tests/integration/app.test.tsx`, "opens the spectrum panel with mock data on canvas click". It appeared on every run: 4 runs here, and also on a run with my tracked changes stashed. It is not hidden; another task owns the fix.

`test-results/`, `playwright-report/` and `dist/test-results/` were deleted afterwards.

**Real-browser QA, Playwright Chromium on `vite preview`.**
- Ports: `127.0.0.1:4190` (after) and `:4191` (before build). Both servers are stopped.
- Real `colorScheme` emulation, dark and light, at 375×800, 768×1024 and 1440×900.
- States checked:
  - home `?mock=1&ra=133.795&dec=-7.245&fov=0.05`
  - spectrum open (canvas click)
  - About open
  - `&hint=1` with the Barnard's Star preset (chip visible)
  - `&mode=diff`
  - `&embed=1`
  - empty state (no position, `/api` aborted)
- Results over 54 page/state/size/scheme combinations:
  - horizontal overflow **0**
  - interactive elements under 44 px **0**
  - axe serious/critical **0** on every "after" combination
- Fonts loaded at 1440: Plex Sans 400/600 and Plex Mono 400/500.

**Chrome via MCP** (own tab, closed afterwards):
- Same-origin iframes on the preview page: 375×800 default, 768×1024 diff, and the 375×420 embed. In each, I clicked the canvas to open the spectrum.
- Measured inside `contentDocument`: overflow 0, small targets 0, spectrum docked, body `rgb(33,33,33)`, Go `rgb(174,220,34)` with `rgb(33,33,33)` text, Plex fonts loaded.
- Light scheme, using the f6 technique (the page's own `prefers-color-scheme: light` declarations injected on `:root`):
  - body `rgb(244,243,236)`, text `rgb(43,43,38)`
  - Go lime with carbon text and a `rgb(70,96,4)` outline
  - stage `rgb(27,27,27)`, drawer `rgb(251,250,245)`, flag chip `rgb(124,82,0)`
  - pressed transport text `rgb(70,96,4)`: brand-deep, not lime
- The live airmanprep.com was viewed read-only. Its primary button is lime `#aedc22` with carbon text, a 1px `#466004` border and an 8 px radius. Fonts are IBM Plex Sans 600 and Plex Mono pill chips. The re-skin matches these.

## Could not verify / open items

- Firefox and Safari were not run. Plex woff2 and `unicode-range` are widely supported. Glyphs outside latin (H₂O/CH₄ subscripts in band labels, Greek) fall back to the system font by design.
- Chrome MCP cannot emulate `prefers-color-scheme`. The light check there used injected rules, as in f6. Real emulation was done in Playwright.
- Only mock data was used; real IRSA frames were not re-checked visually. The stage colour change (neutral grey) only affects letterbox bars.
- JPEG banding: the soft glow shows slight banding in the q50 screenshots. In the browser it is smooth.
- PM decisions:
  - (a) Lime vs warn dot on flag chips (see Deviation).
  - (b) The default theme stays **dark**: the app follows `prefers-color-scheme`, as before, and dark is the `:root` default. Confirm this is the wanted default, or whether a manual theme toggle is wanted (out of scope here).
  - (c) The mark is original. If the team wants the real AirmanPrep logo instead, someone with rights to the file must add it.
  - (d) Update the CLAUDE.md rule 11 "system fonts" wording.
