# Handoff: submission-claims-fix — soften over-precise scientific claims

Status: **done**. Every proper-motion and detection claim in the
judge-facing texts now matches the evidence: the f10 centroid fit has
~2″/yr uncertainty, and the cruder b17 probe produced artefacts
(~135″ shifts → 96″/yr for Epsilon Indi B, 133″/yr for WISE 0855 in
`docs/live-results/*.json`), so no numeric motion claim survives for
those targets. Nothing is stated more firmly than the handoffs allow.

## Files

- `README.md`
- `docs/submission/project-page.md`
- `docs/submission/demo-slides.md`
- `docs/submission/demo-video-script.md`
- `docs/submission/checklist.md` (word-count note only)
- `SI_USAGE.md` (added this task's row)

## Changes (file — old → new)

### `README.md`

- Preset table, Barnard's Star note: "Fastest proper-motion star; motion
  measured in-app" → "Fastest proper-motion star; moves north across
  epochs" (qualitative; the number lives once, in Verified results).
- Preset table, Epsilon Indi B note: "Brown-dwarf binary; motion
  measured in-app" → "Binary brown dwarf (~1.1″ apart), unresolved at
  SPHEREx's 6.15″/pixel; no reliable motion measurement".
- Verified-results bullet: "Proper motion is real and measurable.
  Centroid fits …: Barnard's Star moves **8.9″/yr north** (catalogue
  10.4″/yr); Epsilon Indi B moves **3.3″/yr east, 2.7″/yr south**
  (catalogue 4.0″/2.5″/yr). Diff mode shows …" → "Proper motion is
  real and visible. Barnard's Star moves north, about 9 arcsec per year
  in our simple centroid tracking (catalogue: 10.4 arcsec per year);
  uncertainty about 2 arcsec per year. Diff mode shows it as a clean
  bright/dark dipole. Epsilon Indi B is a binary brown dwarf (about
  1.1 arcsec apart), unresolved at SPHEREx's 6.15 arcsec/pixel; a
  one-click preset, but we did not obtain a reliable motion
  measurement."
- Limitations, WISE 0855 bullet: "…we could not detect it or measure
  its ~8″/yr motion (native pixel ~6.15″). The preset is kept because
  the challenge names it…" → "…not detected convincingly in raw frames.
  Its motion (about 8 arcsec/yr, about 1.3 native pixels at
  6.15 arcsec/pixel) is close to the pixel limit, so we could not
  measure it. The preset exists because the challenge asks to test
  it…".
- Limitations, new bullet: "R Draconis saturates. Its core is flagged
  saturated and reads NaN in 12 of the 24 selected frames (all D4, most
  D2 and D3)…" (f10 finding; README lists limitations, so per task it is
  mentioned there).
- Presets intro: "up to ~4′ off" → "up to ~4.4′ off" (b11 measured
  Barnard ~263″ ≈ 4.4′; the previous figure understated it).

### `docs/submission/project-page.md`

- High-Level Summary: "verified on the real archive: we measured
  Barnard's Star's proper motion in-app" → "verified on the real
  archive, where Barnard's Star visibly moves north". Word count 108 →
  **107** (`wc -w` on the copy block; limit 120). Note + checklist
  updated.
- Embed-URL comment: "its motion is visibly measurable" → "its motion
  is visible across epochs".
- Detailed description: "A centroid fit … measures Barnard's Star's
  proper motion at 8.9″/yr north against the catalogue's 10.4″/yr, and
  Epsilon Indi B's at 3.3″/yr east and 2.7″/yr south against
  4.0″/2.5″/yr — the app measures the effect it was built to show." →
  "In our simple centroid tracking … Barnard's Star moves north at
  about 9 arcsec per year (catalogue: 10.4 arcsec per year); the
  uncertainty is about 2 arcsec per year, and the diff mode shows the
  star as a clean bright/dark dipole. Epsilon Indi B is a binary brown
  dwarf (about 1.1 arcsec apart), unresolved at SPHEREx's
  6.15 arcsec/pixel — a one-click preset, but we did not obtain a
  reliable motion measurement." (Barnard's number now appears once in
  this document.)
- Same paragraph, WISE 0855: "genuinely faint … we could not detect it
  or its motion, and the app says so honestly" → "very faint … not
  detected convincingly in raw frames; its motion (about 8 arcsec/yr,
  about 1.3 native pixels) is close to the pixel limit — the preset
  exists because the challenge asks to test it, and the app says so
  honestly rather than faking a detection."

### `docs/submission/demo-slides.md` (slide 4 + speaker notes)

- Title: "It really works: measured proper motion" → "It really works:
  the motion is real".
- Barnard's bullet: "measured **8.9″/yr north** in-app vs 10.4″/yr
  catalogue" → "moves north, about 9 arcsec per year in our simple
  centroid tracking (catalogue: 10.4 arcsec per year); uncertainty
  about 2 arcsec per year. The diff frame shows the bright/dark dipole
  of a moving source."
- Epsilon Indi B bullet: "measured **3.3″/yr east, 2.7″/yr south** vs
  4.0/2.5″/yr catalogue" → "is a binary brown dwarf (about 1.1 arcsec
  apart), unresolved at SPHEREx's 6.15 arcsec/pixel — a one-click
  preset, but we did not obtain a reliable motion measurement."
- Speaker notes: "the centroid drift of Barnard's Star measures 8.9
  arcseconds per year against a catalogue 10.4." → "in our centroid
  tracking Barnard's Star moves north about 9 arcseconds per year
  (catalogue 10.4, uncertainty about 2)." (37 words, ≤ 40.)

### `docs/submission/demo-video-script.md`

- Caption: "24 of 390 epochs · measured 8.9″/yr (catalogue 10.4″/yr)" →
  "24 of 390 epochs · moves north ~9″/yr (catalogue 10.4″/yr)".
- Facts note: "the in-app centroid fit measures 8.9″/yr north against
  the 10.4″/yr catalogue value" → "a simple centroid fit over the
  series gives about 9″/yr north against the 10.4″/yr catalogue value,
  with an uncertainty of about 2″/yr".

### `SI_USAGE.md`

- Added row for `submission-claims-fix` (this task), same format as
  previous task rows.

## What was NOT changed and why (traceable sources)

- Frame/diff/observation counts, wavelength ranges, detector coverage,
  timing numbers, spectrum point counts (12–24), image sizes
  (177.9/139.5 MiB), build ~36 s, test counts (194/250/62+2), cold vs
  baked times (1–13 min vs 0.2–3.7 s), ~4 MB/~97 % EPSF cutouts,
  bit-identical S3 byte-range reads, 0.3″ preset snap, 192 px grid /
  4 samples per native pixel, 40 s/8 s deadlines, concurrency ≤ 3: all
  traceable to `docs/live-results/*.json` or handoffs b11, b17, b20,
  f10, integration-live, ops-verify-image — kept verbatim.
- WISE 0855 "3–7σ at 3.1–4.6 µm, invisible below ~3 µm": f10 handoff —
  kept.
- "102 infrared bands" / "every six months" (slides, video): public
  SPHEREx mission specification, not an in-app measurement — kept.
- Barnard's Star diff "bright/dark dipole": f10 verified on real data —
  kept.
- The f10 RA-drift figure (−2.7″/yr vs −0.8″/yr catalogue) was never in
  the submission texts; it stays out — within the ~2″/yr uncertainty,
  not a precise measurement.
- All `TODO(team)` markers (team names, links, idea authorship)
  untouched.

## Decisions / deviations

- The prescribed Epsilon Indi B sentence includes "a one-click preset".
  In the README preset table (where it is self-evident) it is shortened
  to "no reliable motion measurement"; the full phrasing is used in the
  Verified-results bullet, project page and slide 4.
- demo-video-script shows the Barnard's figure twice (judge-facing
  caption + internal "facts behind the captions" note). The note exists
  to justify the caption, so the judge sees the claim once.
- "Measured in-app" phrasing removed everywhere; the qualitative
  statements that remain (visibly moves north, diff dipole) are what
  f10 actually observed.

## Open issues

- None new. Pre-existing: `TODO(team)` fields (team name, URLs, repo
  link, SI idea-authorship wording) await the team — unchanged.
- The b17 crude-probe artefacts remain in `docs/live-results/*.json`
  (raw probe output, outside my scope); the texts no longer cite them
  as measurements.

## Verify commands and results

```sh
# no over-precise motion numbers left in judge-facing texts
rg -n '8\.9|3\.3″|2\.7|measured in-app|4\.0″|2\.5″|96\.|13[45]' \
  README.md SI_USAGE.md docs/submission/
# -> only unrelated dependency versions (displaydoc 0.2.7, tokio-macros 2.7.2)

# High-Level Summary word count (copy block only)
sed -n '29,38p' docs/submission/project-page.md | wc -w   # -> 107 (limit 120)

# speaker notes all <= 40 words (python split): max counted 40

# relative-link check over README/SI_USAGE/docs/submission/*.md
# -> no missing files (two glob-pattern false positives: live-results/*.json)

# TODO markers untouched
git diff | rg 'TODO'   # -> no added/removed TODO(team) lines
```

## Merge

```sh
git switch main && git merge --no-ff devin/submission-claims-fix
```
