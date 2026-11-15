# Handoff: b9-perf-tests

**Status: done.** Performance profile + regression budgets for the whole
processing path, tests only. No `src/` changes.

## Files

- `backend/tests/perf_tests.rs` — 7 perf tests (parse, reproject, epoch
  ops, encode+payload, end-to-end, peak memory, concurrency).
- `backend/tests/support/perf.rs` — timing stats, budget helpers,
  synthetic MEF cutout builder (tilted TAN+SIP, WCS-WAVE), synthetic
  `ScienceImage` builder, stub `FrameSource` + router helpers, VmHWM
  reader, contract-shaped payload mirror.

## What was built

- `parse_frame` bench over synthetic MEF cutouts (IMAGE, VARIANCE,
  FLAGS, ZODI, WCS-WAVE; tilted TAN+SIP WCS) at 29/64/128 px, median and
  p95 over 200 runs.
- `reproject` bench (nearest + bilinear) onto 192² and 256² grids.
- `stack_images` / `subtract_epochs` (n-1 diffs vs first, like `diff`
  mode) / `coadd_epochs` (sequential n-1 pairwise chain) for 6/24/60
  frames at 192².
- `image_to_b64` + `serde_json` serialisation of a contract-shaped
  `/frames` payload (24/60 frames, 192/256); raw and gzip sizes measured
  through the real router (`CompressionLayer`, tower-http default
  level = flate2 level 6 — `flate2` itself is not a direct dep).
- End-to-end CPU-only latency through `build_router` + `oneshot` with a
  zero-latency `StubSource`: `/frames` raw/diff/coadd/stack, 24 frames at
  192²; `/spectrum`.
- Peak memory via `/proc/self/status` VmHWM across 60-frame @256
  requests; concurrency: 8 simultaneous `/frames` requests vs
  sequential, plus VmHWM bound.
- Strict budgets gate **release only** (`cfg!(not(debug_assertions))`);
  debug runs use `DEBUG_FACTOR = 40` (memory `4x`) and trimmed
  samples/workloads so `cargo test` stays fast.

## Results (release, i7-13620H, this worktree)

| operation | size | runs | median ms | p95 ms | budget ms |
|---|---|---|---|---|---|
| parse_frame | 29x29 | 200 | 0.02 | 0.03 | 0.5 |
| parse_frame | 64x64 | 200 | 0.02 | 0.03 | 1 |
| parse_frame | 128x128 | 200 | 0.05 | 0.06 | 2 |
| reproject/nearest | 192x192 | 20 | 7.85 | 8.58 | 30 |
| reproject/bilinear | 192x192 | 20 | 10.53 | 11.19 | 40 |
| reproject/nearest | 256x256 | 20 | 14.84 | 15.66 | 60 |
| reproject/bilinear | 256x256 | 20 | 18.89 | 19.68 | 80 |
| stack_images | 6 f @192 | 30 | 0.83 | 1.77 | 5 |
| stack_images | 24 f @192 | 30 | 4.15 | 4.33 | 15 |
| stack_images | 60 f @192 | 30 | 12.61 | 13.41 | 40 |
| subtract_epochs x(n-1) | 6 f @192 | 30 | 0.86 | 0.91 | 5 |
| subtract_epochs x(n-1) | 24 f @192 | 30 | 4.00 | 4.56 | 20 |
| subtract_epochs x(n-1) | 60 f @192 | 30 | 8.89 | 10.44 | 45 |
| coadd_epochs x(n-1) | 6 f @192 | 30 | 2.20 | 3.51 | 8 |
| coadd_epochs x(n-1) | 24 f @192 | 30 | 9.05 | 10.22 | 25 |
| coadd_epochs x(n-1) | 60 f @192 | 30 | 20.75 | 22.67 | 60 |
| image_to_b64 | 192x192 | 50 | 0.07 | 0.09 | 1 |
| image_to_b64 | 256x256 | 50 | 0.13 | 0.14 | 2 |
| serde_json::to_vec | 24 f @192 | 10 | 1.28 | 2.58 | 10 |
| serde_json::to_vec | 24 f @256 | 10 | 2.31 | 5.93 | 12 |
| serde_json::to_vec | 60 f @192 | 10 | 3.24 | 8.34 | 15 |
| serde_json::to_vec | 60 f @256 | 10 | 5.99 | 14.51 | 40 |
| /frames?mode=raw | 24 f @192 | 6 | 220.88 | 239.33 | 800 |
| /frames?mode=diff | 24 f @192 | 6 | 223.43 | 224.30 | 900 |
| /frames?mode=coadd | 24 f @192 | 6 | 234.53 | 238.59 | 900 |
| /frames?mode=stack | 24 f @192 | 6 | 219.39 | 224.09 | 900 |
| /spectrum | 24 f | 6 | 0.06 | 0.19 | 100 |

Payload sizes: 24 f @192 = 4.73 MB raw / 2.64 MB gzip; 60 f @256 =
21.0 MB raw / 11.7 MB gzip (1.8x ratio). Peak memory: VmHWM delta over
three 60 f @256 requests = 0 MiB (whole-process peak ~103 MiB); 8
concurrent 24 f @192 requests = +57 MiB, 4.6x speedup vs sequential.

## Ranked hot spots + optimisation ideas (not implemented)

1. **`reproject` (`src/science/reproject.rs::reproject`, via
   `src/fits/wcs.rs::SkyWcs::radec_to_pixel` + `Poly2::eval`) — ~95% of
   e2e CPU.** ~10 ms/frame @192, ~19 ms @256; the 24-frame e2e (~225 ms)
   is almost exactly 24 reprojections. `Poly2::eval` allocates two
   `Vec<f64>` per call and `radec_to_pixel` calls it ~2×(1+iters) times
   per output pixel (SIP fixed-point inversion) → ~10 mallocs/pixel,
   ~370k per frame @192. Replace the power vectors with fixed arrays
   (`order <= 9` is already enforced at parse) or compute powers
   incrementally; expected 2-4x on reproject. Second lever: the inverse
   SIP for an affine-near WCS converges in ~4 iterations — a Newton step
   or precomputed per-frame inverse could cut it further.
2. **Per-pixel loops in `stack_images`/`subtract_epochs`
   (`src/science/image.rs`, `src/science/epoch.rs`)** — 60 frames @192
   costs ~13/9/21 ms (stack/subtract/coadd-chain). Scalar `[[y,x]]`
   indexing defeats vectorisation; `ndarray::Zip`/slice ops or `rayon`
   over rows would give 3-5x. Low priority: already ~6% of e2e.
3. **Payload size (bandwidth, not CPU)** — 60 f @256 = 21 MB raw,
   11.7 MB gz. Base64 f32-LE inflates 4 B/px to ~5.3 B and floats gzip
   poorly (texture noise). If wire size matters: keep `OUTPUT_NPIX=192`
   (4.7 MB @24 f), or later quantise to f16/u16 or send binary —
   contract change, out of scope here.
4. **Serialisation tail (`image_to_b64` + `serde_json`)** — ~6-9 ms at
   60 f @256; negligible. `image_to_le_bytes` writes per-element; a
   `bytemuck`/slice cast would be faster but buys <10 ms.
5. **`parse_frame`** — <0.1 ms even at 128²; no action needed.
6. **`/spectrum`** — ~0.06 ms for 24 frames; aperture is tiny by design.

## Decisions / deviations

- gzip sizes measured through the router's `CompressionLayer` rather
  than `flate2` (task allowed "tower-http's gzip dependency"; `flate2`
  is only a transitive dep). Level = tower-http default (6).
- `FramesResponseDto` is `pub(crate)`; serialisation is benchmarked on a
  field-for-field mirror in `perf.rs` plus real router requests for the
  byte counts.
- Debug runs scale the e2e/memory/concurrency workloads (6 frames @96)
  — budgets there are smoke checks; strict numbers are release-gated as
  the task requires.
- VmHWM is monotonic per-process: delta over the measured requests is
  0 once the process peak is established, so the budget guards
  *incremental* peak growth; absolute peak (~103 MiB) is reported too.
- Budgets carry ~2-4x headroom over measured p95 (timings are
  machine-dependent; this box is fast).
- `SI_USAGE.md` untouched: outside the allowed scope for this task.

## Open issues

- Budgets are calibrated on one machine (i7-13620H); a much slower CI
  box could flake — headroom is generous but a future task may want
  environment-normalised budgets.
- `perf_6` delta reads 0 when earlier perf tests already set the process
  peak; for a sharper signal it could run in a dedicated process.

## Verify

```
cd backend
cargo fmt --check && cargo clippy --all-targets -- -D warnings   # pass
cargo test                                                     # pass (117 tests; 5 pre-existing ignored)
cargo test --release --test perf_tests -- --nocapture          # 7 pass, ~17 s, table above
cargo test --test perf_tests -- --nocapture                    # 7 pass, ~20 s (relaxed budgets)
```
