# Handoff: b11-presets-epoch (preset targets at SPHEREx epoch)

Status: **done, all gates green.** Agent: Devin. Branch `devin/b11-presets-epoch`. Not pushed.

## Problem

The five presets held ICRS catalogue positions at epoch J2000. Three of the
objects move several arcsec/yr, so at SPHEREx epochs (2025-2026) they were
outside their own FOV (Barnard ~263" north of J2000, WISE 0855 ~125" west,
eps Indi B ~120" away). Presets now store positions propagated to **epoch
2026.0** and larger FOVs for the fast movers.

## Files

| File | Change |
|---|---|
| `backend/src/api/targets.rs` | New epoch-2026.0 coordinates, `fov_deg` 0.1 for the three fast movers, `note` per target with J2000 position, pm and source; provenance documented in the `PRESETS` doc comment. |
| `backend/tests/targets_tests.rs` | **New.** 5 tests, no network: five presets + unique ids, `validate_query_params` + fov bounds, J2000 constants re-propagated to 2026.0 reproduce stored coords within 0.5", `warm_targets()` consistency, linear-vs-rigorous bound (< 1"). |
| `docs/REFERENCES.md` | SIMBAD + Gaia EDR3 / Kirkpatrick 2021 / van Leeuwen 2007 / Dieterich 2018 / astropy-formulation references added. |
| `backend/tests/api_tests.rs` | **Deviation, see below.** `targets_returns_five_presets` asserted the old J2000 values; updated to the new ones. Required for `cargo test` to pass (definition of done). |

## Astrometry (SIMBAD TAP, ICRS epoch J2000, queried 2026-11-14 via the
## CFA mirror `simbad.cfa.harvard.edu` — `simbad.cds.unistra.fr` sim-tap was 404)

| id | ra J2000 (deg) | dec J2000 (deg) | plx mas | pmra* mas/yr | pmdec mas/yr | rv km/s | source |
|---|---|---|---|---|---|---|---|
| barnards-star (GJ 699) | 269.4520769586187 | +4.693364966576667 | 546.9759 | -801.551 | +10362.394 | -110.11 | Gaia EDR3 (2020yCat.1350....0G) |
| epsilon-indi-b (* eps Ind B) | 331.04414658852124 | -56.782844195116105 | 270.658 | +3981.977 | -2466.832 | — (0 used) | Gaia EDR3 |
| wise-0855 (WISEA J085510.74-071442.5) | 133.81890825 | -7.247048 | 439.0 | -8123.7 | +673.2 | — (0 used) | Kirkpatrick et al. 2021 (2021ApJS..253....7K) |
| rw-aurigae (V* RW Aur) | 76.95652603458333 | +30.401438173888888 | 15.42 | +7.23 | -20.33 | +15.0 | van Leeuwen 2007 (2007A&A...474..653V) |
| r-draconis (V* R Dra) | 248.16761083295 | +66.75497941722 | 1.2509 | -20.643 | +6.149 | -132.8 | Gaia EDR3 |

Notes on the data:

- WISE 0855 **is** in SIMBAD (main_id `WISEA J085510.74-071442.5`) with
  Kirkpatrick et al. 2021 astrometry, so the literature values were used as
  planned but fetched through SIMBAD like the rest. Consistency check:
  propagating the SIMBAD J2000 position forward with the published pm lands
  within ~1" of the AllWISE-epoch official coordinates at 2010.5 — i.e. the
  challenge coordinates are the AllWISE epoch (~2010.5), as suspected.
- eps Indi B has no separate SIMBAD entries for Ba/Bb; the Gaia EDR3 entry
  is component **Ba** (T1). Ba-Bb projected separation ≈ 1.1" at ~2026
  (orbit of Dieterich et al. 2018, AJ 156, 103) — **unresolved at
  6.15"/px**. rv assumed 0 (the pair shares eps Ind A's ~-40 km/s; effect
  < 0.05" over the span).
- WISE 0855 rv unmeasured — assumed 0; same negligible effect.

## Propagation

Rigorous linear 3-D space motion (uniform velocity, same formulation as
astropy `apply_space_motion`; ~20 lines, so used for all five — it is free
accuracy). Radial velocity used where SIMBAD has it; 0 for eps Indi B and
WISE 0855 (unmeasured, negligible). The naive tangent-plane linear formula
the task allowed as fallback agrees within 0.44" (Barnard worst; 0.06" eps
Indi B, 0.01" WISE) — bound checked by a dedicated test.

Validation of the code path: propagating SIMBAD J2000 -> 2016.0 for
Barnard reproduces the Gaia EDR3 epoch-2016.0 catalogue position within
**0.16"** (pm/plx identical to Gaia's), so the formula is sound.

## Barnard check vs live-measured position

Propagated to ~2025.9: **RA 269.44628, Dec +4.76804**. Live-measured
(integration-live handoff): RA 269.4464, Dec +4.7667.
Difference: **dRA* -0.4", dDec +4.8", total 4.8"** — within "a few arcsec".
Caveat: the integration agent's value is itself a propagation (they noted
the detection was within ~2" of it), not an independent astrometric
measurement, so part of the 4.8" is their propagation constants.

## For the frontend teammate — drop-in for `frontend/src/lib/presets.ts`

`fov` fields for the three movers changed to 0.1; `ra`/`dec` strings are
epoch-2026.0 sexagesimal identical to the backend table; decimal degrees
given for reference (the parser accepts either form).

```ts
export const PRESET_TARGETS: readonly PresetTarget[] = [
  {
    id: "barnards-star",
    label: "Barnard's Star",
    ra: "17h57m47.102s",            // 269.4462583
    dec: "+04d46m05.97s",           // +4.7683250
    fov: 0.1,
    note: "SIMBAD (Gaia EDR3) J2000: 269.45208, +4.69336, pm -801.55/+10362.39 mas/yr; position propagated to epoch 2026.0",
  },
  {
    id: "epsilon-indi-b",
    label: "Epsilon Indi B",
    ra: "22h04m23.201s",            // 331.0966708
    dec: "-56d48m02.34s",           // -56.8006500
    fov: 0.1,
    note: "Brown-dwarf binary Ba+Bb (~1.1 arcsec apart, unresolved at 6.15 arcsec/px). SIMBAD (Gaia EDR3) J2000, pm +3981.98/-2466.83 mas/yr; propagated to epoch 2026.0",
  },
  {
    id: "wise-0855",
    label: "WISE 0855",
    ra: "08h55m02.344s",            // 133.7597667
    dec: "-07d14m31.86s",           // -7.2421833
    fov: 0.1,
    note: "SIMBAD (Kirkpatrick et al. 2021) J2000: 133.81891, -7.24705, pm -8123.7/+673.2 mas/yr; propagated to epoch 2026.0. The challenge coordinates are the AllWISE ~2010.5 epoch",
  },
  {
    id: "rw-aurigae",
    label: "RW Aurigae",
    ra: "05h07m49.581s",            // 76.9565875
    dec: "+30d24m04.65s",           // +30.4012917
    fov: 0.05,
    note: "SIMBAD (Hipparcos, van Leeuwen 2007) J2000: 76.95653, +30.40144, pm +7.23/-20.33 mas/yr; propagated to epoch 2026.0",
  },
  {
    id: "r-draconis",
    label: "R Draconis",
    ra: "16h32m40.136s",            // 248.1672333
    dec: "+66d45m18.09s",           // +66.7550250
    fov: 0.05,
    note: "SIMBAD (Gaia EDR3) J2000: 248.16761, +66.75498, pm -20.64/+6.15 mas/yr; propagated to epoch 2026.0",
  },
];
```

## Expected on-sky motion (for the demo script, native pixel 6.15")

| id | mu ("/yr on sky) | px/yr | direction | over ~1 yr of survey epochs |
|---|---|---|---|---|
| barnards-star | 10.39 (ra* -0.80, dec +10.36) | 1.69 | almost due north | ~10.4" ≈ 1.7 px — visible with centroiding (integration agent measured +11.1"/yr N) |
| wise-0855 | 8.15 (ra* -8.12, dec +0.67) | 1.33 | almost due west | ~8" ≈ 1.3 px |
| epsilon-indi-b | 4.68 (ra* +3.98, dec -2.47) | 0.76 | ESE | ~4.7" ≈ 0.8 px — marginal by eye |
| rw-aurigae | 0.022 | 0.004 | — | none (it is a variable star, not a mover) |
| r-draconis | 0.022 | 0.004 | — | none (Mira variable) |

## Decisions / deviations

1. **`backend/tests/api_tests.rs` edited although outside the allowed
   scope.** Its `targets_returns_five_presets` hard-codes the old J2000
   values; leaving it would break the definition-of-done `cargo test`.
   Minimal change: expected values swapped to epoch-2026.0 ones. Flag for
   the orchestrator — if scope was meant literally, revert and relax the
   test instead.
2. **`SI_USAGE.md` not edited** (outside scope). SI-generated content in
   this task: the propagation script logic, `targets.rs`, `targets_tests.rs`,
   `api_tests.rs` expectations, REFERENCES additions and this handoff.
   Astrometric data itself is from SIMBAD/literature, not generated.
3. **SIMBAD main TAP was 404** at query time; used the official CFA mirror
   `simbad.cfa.harvard.edu` (same `basic` schema). Documented in REFERENCES.
4. FOVs: 0.1 for the three fast movers (±180" half-side ≫ ±10.4"/yr drift),
   0.05 kept for the two variables, per task. All presets keep their ids.
5. Sexagesimal strings kept (same representation as `presets.ts`, same
   parse-then-validate path); precision 0.001s RA / 0.01" Dec, well inside
   the 0.5" propagation check tolerance.

## Open issues

- None blocking. If IRSA is queried at the new centres: Barnard and WISE
  already verified live by integration-live at ~these coordinates; eps
  Indi B centre is new (~121" from the old preset) — first live request
  will confirm coverage.
- eps Indi B's rv is assumed 0; using eps Ind A's ~-40 km/s changes the
  stored position by < 0.1" — safe to ignore.

## Verify

```
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
# fmt: clean. clippy: clean (0 warnings).
# test: all suites pass — 127 passed total, 5 ignored (pre-existing b6 known bugs),
#   targets_tests.rs: 5/5 ok; api_tests targets_returns_five_presets ok.
```
