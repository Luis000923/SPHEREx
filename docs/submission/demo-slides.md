# Demo slide deck — 7-slide outline

Rules that constrain this deck: **≤ 7 slides total, including the
title/intro slide**; slides must show the team name, member names and the
chosen challenge; they must show the project image, the UX, and how NASA /
partner data make it possible. All text in English.

Real screenshots already exist under [`../screens-real/`](../screens-real/)
— the filenames below are the ones verified in end-to-end QA (handoff
`f10-real-data-qa`). Speaker notes are ≤ 40 words each — keep them that
way when rehearsing.

---

## Slide 1 — Title

- **Team name:** `TODO(team): <team name>`
- **Members:** `TODO(team): <member names, as registered on the team>`
- **Challenge:** Planet X and SPHEREx — NASA Space Apps Challenge 2026
- **Project:** SPHEREx Blink
- Tagline: "A time-lapse window into the SPHEREx all-sky survey."
- Image: `../screens-real/barnards-star-1440-raw-spectrum-field-star.jpg`
  — the app showing Barnard's Star, the spectrum panel and real FITS flag
  badges.

**Speaker notes (≈30 words):**
We are `<team name>` and this is SPHEREx Blink, our answer to Planet X and
SPHEREx: a public web tool that turns the archive into a blink comparator
anyone can use.

---

## Slide 2 — The problem

- SPHEREx maps the whole sky every six months in 102 infrared bands, but the
  data lives in multi-extension FITS files behind archive APIs.
- Finding moving or variable objects means comparing epochs — until now an
  expert-only workflow.
- No public, instant answer to "does anything change at this position?".
- Image: a plain FITS header or IRSA archive-table screenshot, visually dry
  on purpose (any dry capture works; none is shipped, reuse an app frame if
  short on time).

**Speaker notes (≈35 words):**
SPHEREx images the entire sky twice a year, but every product is an expert
format. Blinking epochs is how Pluto and the Backyard Worlds planets were
found — yet nobody without FITS skills could try it on SPHEREx.

---

## Slide 3 — The solution: blink the sky

- One sentence: type coordinates, get a time-lapse of every SPHEREx epoch.
- Playback 1–30 fps, scrubber, and a two-frame blink comparator.
- Comparison modes: raw, epoch difference, co-add, per-detector stack.
- Image: `../screens-real/epsilon-indi-b-1440-raw-spectrum.jpg` — the
  viewer playing a real 24-epoch series of Epsilon Indi B (24 frames
  selected out of 488 archive observations).

**Speaker notes (≈35 words):**
SPHEREx Blink does the expert workflow in the browser: we fetch every
exposure of your patch and play it as a time-lapse. Blink, difference or
stack — moving sources announce themselves.

---

## Slide 4 — It really works: the motion is real

- Barnard's Star moves north, about 9 arcsec per year in our simple
  centroid tracking (catalogue: 10.4 arcsec per year); uncertainty about
  2 arcsec per year. The diff frame shows the bright/dark dipole of a
  moving source.
- Epsilon Indi B is a binary brown dwarf (about 1.1 arcsec apart),
  unresolved at SPHEREx's 6.15 arcsec/pixel — a one-click preset, but we
  did not obtain a reliable motion measurement.
- Honest counter-example: WISE 0855 (a Y dwarf) is only 3–7σ in single
  SPHEREx frames — we show it and say it is not detected, no faking.
- Image: `../screens-real/barnards-star-1440-diff-after-fix-north-up.jpg`
  — the diff dipole of Barnard's Star (north up, east left).

**Speaker notes (≈37 words):**
This is not a mockup: in our centroid tracking Barnard's Star moves
north about 9 arcseconds per year (catalogue 10.4, uncertainty about 2).
WISE 0855 is too faint for single frames — the app reports that
honestly.

---

## Slide 5 — More than a movie: spectra, flags, overlays

- Click any pixel → spectrum assembled from every covering exposure (12–24
  points measured per preset), with error bars and molecular-ice band
  markers.
- Per-frame metadata: epoch, detector, wavelength, flag badges read from
  the FITS headers — not hardcoded.
- Optional Gaia DR3 / AllWISE catalog overlay.
- Image: `../screens-real/r-draconis-1440-raw-saturated-spectrum.jpg` —
  spectrum panel open; flag badges show the star's saturated core honestly.

**Speaker notes (≈35 words):**
Click a pixel and we assemble its spectrum from every exposure, with
uncertainties and instrument flags read from the FITS headers themselves.
Gaia and AllWISE markers tell you what is already catalogued there.

---

## Slide 6 — How NASA data makes it possible

- IRSA SIA2 cone search (TAP fallback) → every epoch, fetched live.
- Cutouts as byte ranges from the AWS Open Data mirror
  `nasa-irsa-spherex`, bit-identical to the IBE cutout (IBE is the
  fallback).
- FITS IMAGE / VARIANCE / FLAGS / ZODI / WCS-WAVE decoded in our own
  bounds-checked reader.
- Partner data: Gaia DR3 and AllWISE via CDS VizieR and Aladin Lite.
- Polite client: concurrency ≤ 3 on IRSA, retries, caches, deadline —
  IRSA stays a shared public service.
- Image: `../screens-real/barnards-star-1440-overlay-fallback-loaded.jpg`
  — the catalog overlay running over real data.

**Speaker notes (≈40 words):**
Everything on screen is real archive data: IRSA's SIA2 and TAP services
find the epochs, byte-range reads on the open S3 mirror fetch only what
is needed, and we decode the full FITS product. Gaia and AllWISE come
through CDS.

---

## Slide 7 — Anyone, anywhere: embed, share, grow

- Every view is a URL — share it, or embed the live viewer in an iframe.
- The demo never depends on the network: the image ships the five presets
  baked in and was verified serving real data with `--network none`.
- New SPHEREx releases are adopted by configuration, no code change.
- Open source, Apache-2.0. Try it: `TODO(team): <deployed URL>`.
- Image: `../screens-real/barnards-star-375-768-embed-iframes.jpg` — the
  app at 375 px, 768 px and inside a 375 px embed iframe.

**Speaker notes (≈35 words):**
A discovery view is a link, so teachers, amateurs and researchers share
the same sky. Future data releases need only configuration. SPHEREx Blink
is open source — the URL is on screen.

---

## Production notes (not part of the deck)

- Suggested tools: reveal.js / Markdown slides, Google Slides or plain
  HTML — any format the team can export to a public, login-free link.
- Keep each slide to one idea; the image is the hero (matches the app's
  own design rule).
- All screenshots are real output; reusing `../screens-real/` files keeps
  the deck honest and consistent with the README.
- Verify the final public link in a private/incognito window before
  submitting (see `checklist.md`).
