# Handoff: b13-diff-selection

Status: **done**. All gates green (`fmt`, `clippy -D warnings`, 161 tests incl. 10 new; 1 live check opt-in).

## Problem

`diff`/`coadd`/`stack` only merge frames of the same detector AND
wavelength within 0.05 um (`science::group_frames`). The epoch
selection (`select_frames`) spread the 24-frame budget evenly over the
whole survey, so the selected frames landed in mostly singleton groups
and the combining modes starved (measured live on WISE 0855: ~5 usable
diff frames out of 24).

## Files

- `backend/src/irsa/sia.rs` — new pre-download metadata accessors and
  the grouped selector on `Observation`:
  - `detector_hint()`: parses `energy_bandpassname` (`"SPHEREx-D3"` ->
    3). SIA2 has no detector column; this is the only detector signal
    available before the cutout download (the FITS `DETECTOR` keyword
    is authoritative but only readable afterwards — used for grouping,
    not for selection).
  - `band_center_um()`: midpoint of `em_min`/`em_max` — pre-download
    estimate of the frame's central wavelength.
  - `Observation::select_comparable(obs, max)`: mode-aware selection
    for the combining modes (algorithm documented in the doc comment):
    bucket by detector hint, cluster band centres into 0.05 um bins
    with the same anchored rule `group_frames` applies (NaN centres =
    singleton bins, mirroring `group_frames`), rank buckets by epoch
    count then baseline, fall back to the even-spread `select_frames`
    when the best bucket has < 3 epochs, else fill the budget
    bucket-by-bucket (all epochs of a bucket that fits, else
    `remaining` epochs evenly spread over its baseline). Newest-first
    output, same contract as `select_frames`.
  - `COMPARABLE_WAVELENGTH_BIN_UM` aliases
    `science::DEFAULT_WAVELENGTH_TOLERANCE_UM` so the pre-download bins
    exactly approximate the post-download groups.
- `backend/src/api/frames.rs` — `/frames` picks
  `Observation::select_comparable` for `mode=diff|coadd|stack`,
  `select_frames` for `mode=raw`. `total_observations`, `truncated`,
  DTOs and the contract are unchanged.
- `backend/tests/diff_selection_tests.rs` — 10 tests (all synthetic;
  one opt-in live check via `SPHEREX_LIVE=1`).

## Decisions / deviations

- **`select_comparable` is an associated function on `Observation`**,
  not a free `pub fn select_comparable_frames` in `sia.rs`: `mod sia`
  is private to `crate::irsa` and `irsa/mod.rs` (the re-export point)
  is outside the allowed scope. `Observation` is already re-exported,
  so the selector stays reachable from `api` and integration tests.
- **`select_frames` signature and behaviour untouched**: `warm`
  (`fetch_frames_n`) and `/spectrum` keep calling it, so the warm-up
  warms exactly the default-mode (raw) selection, unchanged, and raw
  keeps the even spread it needs as the time-lapse.
- **Ranking**: epoch count first, then baseline. When the best bucket
  has < 3 epochs the old spread selection is used (a diff needs two
  members to yield anything). Unknown-detector rows share a "none"
  detector bucket per wavelength — they may still be comparable.
- **Stack also uses the (detector, wavelength) buckets** per the task
  spec; `stack_detectors` is wavelength-agnostic inside a detector,
  so concentrating on same-λ epochs is a strict subset of what it can
  merge — still optimal budget use.

## Before / after

Synthetic (`comparable_selection_maximises_group_size_and_baseline`,
76 obs, dominant bucket = 30 epochs of D3 @ 2.0 um over 400 d):

- even spread (`select_frames`, old behaviour): largest comparable
  group in the 24-frame selection = **10 epochs**.
- `select_comparable` (new): largest group = **24 epochs**, baseline
  400 d (>= 80% of the bucket's baseline). Diff would yield ~9 frames
  before, 23 after.

Live (`SPHEREX_LIVE=1 cargo test --test diff_selection_tests live_`,
2026-11-14, one `client.search` = 2 SIA requests, no cutouts):

```
WISE 0855: 280 observations; even spread -> largest comparable group
7 epochs (362 d baseline); comparable selection -> 24 epochs (373 d)
```

i.e. the diff-mode input set went from a 7-epoch group (≈6 diffs) to a
24-epoch group (≈23 diffs) at the same 24-frame budget.

## Open issues

- `select_comparable` trusts `energy_bandpassname` for the detector;
  rows whose band name lacks `D<n>` are bucketed as "unknown". If a
  release ever drops or renames the column the worst case is the
  fallback spread — safe, but worth a mention.
- The warm-up only warms the raw selection (per spec). The first diff
  request on a target may still pay cold-cutout latency on its
  (different) 24-frame set; the cache keeps them for retries.

## Verify

```
cd backend
cargo fmt --check                          # ok
cargo clippy --all-targets -- -D warnings  # ok
cargo test                                 # 161 pass, 0 fail
SPHEREX_LIVE=1 cargo test --test diff_selection_tests live_   # opt-in
```
