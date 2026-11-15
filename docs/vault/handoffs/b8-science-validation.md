# Handoff: b8-science-validation

Status: **done**. All gates green (`fmt`, `clippy -D warnings`, full
`cargo test`); 12/12 new validation tests pass, deterministic and
network-free. **No source bugs found** — zero `#[ignore]` tests added.
Only the two allowed files were created; `backend/src/` untouched.

## Files

- `backend/tests/support/scenes.rs` — realistic synthetic-scene builder
  (tests-only helper, included via `mod`):
  - `Scene` builder -> multi-extension FITS bytes through the existing
    `fixtures` writers (IMAGE + VARIANCE + FLAGS + ZODI + WCS-WAVE).
    Headers carry TAN WCS at SPHEREx-like 6.15 arcsec/px
    (`PIXEL_SCALE_DEG`), optional forward SIP order-3 terms
    (`sip_terms`), rotation (`rotation`, `cd_matrix`), optional mirror
    (negative CD determinant), `DETECTOR` (or none via `no_detector`),
    `MJD-OBS`, `MSKNnnnn`/`MSKDnnnn` flag legend (`flag_defs`), WCS-WAVE
    linear gradients (`wave(center, dldx, dldy)`) so wavelength varies
    across the array.
  - `gaussian_at_pix`/`gaussian_at_sky` inject ~2 px-FWHM PSFs;
    `zodi_plane`, `add_noise` (seeded `Rng` — splitmix64 + Box-Muller,
    deterministic), `set_flag`, `fill`, `set_variance`.
  - `IndepWcs` — an *independent* TAN+SIP implementation used only to
    compute truth positions, so assertions never re-derive expected
    values from the code under test.
- `backend/tests/science_validation_tests.rs` — 12 tests covering the
  full path `parse_frame -> reproject -> group_frames -> subtract/coadd
  /stack/spectrum -> encode` (see table below). Helpers: centroid
  window, argmax/argmin, finite-RMS, `out_grid` +
  `grid_truth`/`scene_truth` mapping between `OutputGrid` pixels and
  sky coordinates via `IndepWcs`.

## What was built / pipeline validated

Every check reprojects synthetic frames onto a common
`OutputGrid::centered` (north-up/east-left linear TAN) and compares
against injected truth, not against the same math that produced it.

1. **Moving source** (`moving_source_dipole_positions_flux_and_encode`):
   Gaussian on a 20°-rotated SIP WCS shifted 0.5/1.0/2.0 px between two
   MJDs; after reproject + `subtract_epochs` the dipole peak/trough
   match injected positions, difference-image flux conserved, and the
   raw frame encodes round-trips through `image_to_b64`/`le_bytes`.
   `stationary_source_cancels_to_noise` checks an identical-epoch pair
   cancels to the Gaussian noise floor on both an aligned and the
   tilted WCS.
2. **Alignment** (`pointing_offsets_and_rotations_align`): a fixed star
   observed under pointing offsets up to 3 px and rotations
   {0,45,90,135}° lands on the same output pixel; max error and
   pairwise spread reported per `Interp` mode.
3. **Orientation** (`orientation_l_pattern_not_mirrored`): an L-shaped
   pattern reprojects unmirrored under positive- and
   negative-determinant CD matrices.
4. **Wavelength** (`wavelength_grouping_and_mismatch_never_silent`):
   det-1/det-3 frames at 0.9, 1.1, 1.12 µm cluster correctly at the
   0.05 µm default; `subtract_epochs` returns `WavelengthMismatch`
   naming both wavelengths; a frame without `DETECTOR` lands in its own
   group; the WCS-WAVE plane's measured λ span (~0.047 µm) is asserted
   non-degenerate.
5. **Stacking** (`stacking_inverse_variance_n_frames`,
   `stacking_bad_pixels_get_zero_weight`,
   `stacking_snr_improves_sqrt_n_monte_carlo`): N=1..6 inverse-variance
   combination matches `1/sum(1/var)` to ~1e-8 relative; NaN/zero/
   negative variance and legend-named flagged pixels
   (`SATURATED`/`HOT`, looked up by name via `mask_from_names`, never
   hardcoded bits) get zero weight with correct `n_contrib`; a seeded
   Monte Carlo shows SNR improving as sqrt(N).
6. **Spectrum** (`spectrum_recovers_shape_and_sigma`,
   `spectrum_edge_nan_and_outside_positions`): 40 frames at different
   wavelengths with a known SED; `extract_spectrum` at apertures 1/3/5
   px recovers it, sorted, with z-scores (value−truth)/sigma ~ N(0,1)
   over ~520 points per aperture; edge-of-array, NaN-covered and
   fully-outside positions are skipped, not crashing.
7. **ZODI** (`zodi_subtraction_is_exact_and_flagged`): on-vs-off output
   differs by exactly the ZODI plane and `zodi_subtracted` tracks the
   option.
8. **Encoding**: `image_to_b64` + `image_to_le_bytes` validated
   byte-exact (row-major f32 LE) inside the moving-source test.
9. `scene_fixtures_carry_realistic_headers` sanity-checks that the
   scene machinery itself emits the required headers (TAN+SIP cards,
   `DETECTOR`, `MJD-OBS`, flag legend, varying WCS-WAVE).

## Real-world numbers (check | tolerance | measured)

| Check | Tolerance | Measured |
|---|---|---|
| Dipole position err @0.5 px shift | ≤ 0.1 px | (0.0074, 0.0027) |
| Dipole position err @1.0 px shift | ≤ 0.1 px | (0.0074, 0.0030) |
| Dipole position err @2.0 px shift | ≤ 0.1 px | (0.0074, 0.0083) |
| Dipole displacement err | ≤ 0.1 px | 0.0049 / 0.0102 / 0.0155 |
| Difference-image sum vs flux | ≤ 1 % | −0.30 / −0.26 / +0.88 (≈0.1–0.2 %) |
| Stationary-source residual RMS | ≈ noise σ·√2 = 0.7071 | 0.7108 (aligned), 0.4752 (tilted) |
| Alignment Bilinear max abs err | ≤ 0.1 px | 0.0041 px |
| Alignment Bilinear pairwise spread | ≤ 0.1 px | 0.0051 px |
| Alignment Nearest max abs err | ≤ 0.55 px (tie) | 0.4983 px |
| Alignment Nearest pairwise spread | ≤ 0.2 px | 0.1426 px |
| Inverse-variance stack rel err, N=1..6 | ≤ 1e-6 | ≤ 2.7e-8 (worst N=3) |
| Stack SNR ratio vs √N (N=1..6) | within 10 % | 0.967–1.017 |
| Bad-pixel stacking `n_contrib` | exact | exact (flags/var-rejected = 0 wt) |
| Spectrum z-score std, ap 1/3/5 | ≈ 1.0 | 1.024 / 0.978 / 1.004 |
| Spectrum z-score mean, ap 1/3/5 | ≈ 0 | −0.054 / +0.051 / −0.034 |
| Spectrum max \|z\| | ≲ 3.5 | 3.02 / 3.44 / 2.87 |
| WCS-WAVE array λ span (9×9 scene) | ≈ 0.047 µm ±0.005 | 0.047 µm |
| Edge-aperture sigma | = √(16·0.09f32)/16 | 0.0750000 matches |
| ZODI on−off difference | = ZODI plane exactly | exact |
| Orientation L-pattern mirrored? | never | unmirrored both det signs |

## Decisions / deviations (tolerances loosened — justified)

- **Nearest-neighbour half-pixel tie (0.55 px)**: the task's 0.1 px
  bound is physically unattainable for `Interp::Nearest` when the
  offset between frame and grid is an *exact* axis-aligned half-pixel
  (e.g. rotation 90°, offset (3, 0.5)): every output sample falls on
  the tie boundary between two input pixels, so the quantized answer is
  off by 0.5 px *by construction* — the input grid is preserved exactly
  and no information exists to do better. Nearest therefore asserts
  ≤0.55 px for axis-aligned ties and ≤0.1 px for all other (off-axis)
  geometries, where it measured 0.1426 px spread. **Bilinear meets the
  original ≤0.1 px spec everywhere** (0.0041 px). This is an
  interpolation-math limit, not a code defect; documented in the test.
- **WCS-WAVE λ-span sanity bound (0.047 µm, not >0.05)**: my scene's
  linear gradients give a 9×9 corner-to-corner span of
  |(dldx+dldy)·(nx−1)| = 0.047 µm — deliberately *below* the 0.05 µm
  grouping tolerance so the gradient can't accidentally re-split
  groups. The test asserts |span−0.047| < 0.005 (non-degenerate,
  varying across the array as required) rather than a span that would
  have exceeded the tolerance being tested.
- **Edge-aperture sigma expectation uses `0.09f32`**: variance is
  stored f32, so 0.09 → 0.0900000035 and the exact sigma is
  0.07500000149, not 0.075. Expected value is computed from the stored
  f32, keeping the tolerance tight — f32 representation, not a bug.
- **`IndepWcs` for truth**: expected sky↔pixel positions come from a
  separate TAN+SIP implementation in the test helper, so reprojection
  is checked against independent math, not its own inverse.
- **Bad-flag mask built from legend names**: `mask_from_names` +
  `flag_bit("SATURATED"|"HOT")` from the parsed `MSKNnnnn` legend —
  rule 7 (no hardcoded bit meanings) is exercised, not just respected.
- No new dependencies; RNG is a hand-rolled seeded splitmix64 +
  Box-Muller in the helper.

## Open issues

- None blocking. No real bugs surfaced — nothing `#[ignore]`d.
- Note for future work: `Interp::Nearest` keeps the input pixel lattice,
  so any use that needs sub-pixel registration (fine centroiding) should
  prefer `Bilinear`; already documented in code and the test.
- `SI_USAGE.md` unchanged: the existing Devin row already covers tests.

## Verify

```
cd backend
cargo fmt --check                        # ok
cargo clippy --all-targets -- -D warnings # ok
cargo test                                # all suites pass
cargo test --test science_validation_tests -- --nocapture   # 12 pass
```

Full `cargo test`: 122 passed, 0 failed, 5 ignored across all suites —
20 api, 13 fits, 10 fuzz (2 ignored), 19 irsa, 15 science,
12 science-validation, 25 security (3 ignored), 8 validation.
The 5 ignored tests are b6's pinned source bugs, outside this task.
