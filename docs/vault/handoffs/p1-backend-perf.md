# Handoff: p1-backend-perf — backend response-time optimisation

Branch: `devin/p1-backend-perf`
Status: **done** — all gates pass, every optimisation was measured before/after.

## Files changed

| File | Change |
|---|---|
| `backend/Cargo.toml` | `rayon = "1.10"` dep; `[profile.release]` `lto = "fat"`, `codegen-units = 1`, `strip = true` |
| `backend/Cargo.lock` | rayon + transitive deps (scope exception: required by the new dep, mechanically generated) |
| `backend/src/science/reproject.rs` | rewritten: hoisted per-call/per-row WCS invariants, linear input-WCS pre-skip with auto-calibrated SIP margin, slice-based sampling, per-row rayon parallelism |
| `backend/src/science/image.rs` | `stack_images`: flat-slice fast path + scalar fallback, per-row rayon parallelism above 64 k·px·frames |
| `backend/src/science/epoch.rs` | `subtract_epochs`: same treatment (8-plane slice match, `SubRow` rows, parallel above 8 kpx) |
| `backend/src/api/frames.rs` | `spawn_blocking` around reproject/compose/encode; `par_iter` over cutouts; process-wide `moka::future::Cache` of serialized responses keyed by `RespKey` (params + 64-bit FNV fingerprint of decoded inputs); `ResponseCacheStats` counters |
| `backend/src/api/mod.rs` | `from_config` sizes the response cache from `MAX_CACHE_MB`; re-export `response_cache_stats` |
| `backend/tests/perf_tests.rs` | frozen pre-p1 `reproject`/`stack`/`subtract` implementations + `perf_8_*` regression tests (tol 1e-6); `perf_9` cache test; `perf_7` now uses 8 distinct URIs (identical ones would measure the cache, not concurrency) |
| `SI_USAGE.md` | one row for this task (scope exception: repo rule 9 requires it) |

## What was built

1. **Release profile**: `lto = "fat"`, `codegen-units = 1`, `strip = true`.
   `panic = "abort"` was **rejected**: the task conditions it on tests
   passing, and `cargo test --release` needs `catch_unwind` — any
   panicking test would abort the whole harness. Binary size
   10,569,912 B → 8,059,064 B (-24 %).
2. **reproject.rs** (~7× at 256²): the output WCS is linear so
   `pixel_to_radec` is evaluated as two matrix rows (per-call hoisted
   invariants, per-row affine terms); a linear estimate of the input
   `radec_to_pixel` with a SIP margin calibrated from the real WCS
   corners skips clearly-outside output pixels; sampling runs on
   contiguous slices; output rows run on rayon. Same f64 op order →
   bitwise-identical output (frozen-reference test pins 1e-6).
3. **Epoch ops** (`stack_images`, `subtract_epochs`; `coadd` delegates
   to stack): contiguous-plane fast path, per-row rayon, identical
   per-pixel accumulation order → identical results.
4. **Handler**: reproject/compose/encode moved into
   `tokio::task::spawn_blocking`; frames reproject on `par_iter`.
   Concurrent bursts now scale (1.2× → 3.6×; wall 78 → 19 ms).
5. **Response cache**: `moka::future::Cache<RespKey, Bytes>`,
   byte-weighted, sized in `AppState::from_config` as `MAX_CACHE_MB/4`
   (clamped 4–64 MiB, default 32 MiB when states are built by literal).
   The key carries a content fingerprint of the decoded inputs (urls,
   WCS, sampled raster data) so a hit provably reproduces the same
   bytes — no cross-source contamination, no stale answers when the
   upstream set changes. Bodies computed under a live deadline
   (`deadline_hit`) are not cached. Warm hit: **~0.06 ms**.

## Decisions / deviations

- `panic = "abort"` omitted (would break the test harness — see above).
- Response cache is a process-wide `static` in `frames.rs`, not an
  `AppState` field: `AppState` is built by struct literal in test
  helpers outside this task's allowed scope; a new field would break
  them. Sized once from `AppState::from_config`.
- `perf_7` uses 8 distinct `fov` values: with a response cache, 8
  identical concurrent URIs would measure cache hits, not concurrency.
- `moka::future::Cache` (feature `future`, already enabled) instead of
  `sync` — no Cargo.toml feature change needed.
- `SI_USAGE.md` row added outside the listed scope, per repo rule 9.
- Concurrent identical cold requests each compute once (no
  `try_get_with` coalescing) — bounded transient duplication, keeps
  error identity exact.

## Open issues

- `/spectrum` still reprojects on the async executor path
  (out of scope: `spectrum.rs` not in allowed files).
- Response-cache hit coalescing for concurrent cold misses could be
  added later (`try_get_with`) if real traffic bursts identical URIs.

## Verification

```
cd backend
cargo fmt --check                                   # ok
cargo clippy --all-targets -- -D warnings           # ok
cargo test                                          # 218 tests, all pass
cargo test --release --test perf_tests -- --nocapture  # 10 tests, all pass
```

New tests: `perf_8_frozen_reference_reproject` (SIP/no-SIP × 2 grids ×
nearest/bilinear × zodi on/off), `perf_8_frozen_reference_epochs`
(stack 12×96, subtract, coadd vs verbatim pre-p1 code, tol 1e-6),
`perf_9_response_cache` (byte-identical body, hit counter).

## Before / after (same machine, release, 16 cores)

| Measurement | Before | After | Note |
|---|---|---|---|
| reproject bilinear 29→256² | 18.67 ms | 2.89 ms (p95 3.31) | LTO alone: 15.85 ms |
| reproject bilinear 29→192² | ~12 ms | 1.58 ms | |
| reproject nearest 29→256² | — | 2.46 ms | |
| stack 24 frames @192² | 3.78 ms | 0.47 ms | scalar→slices+rayon |
| subtract 24 pairs @192² | 3.77 ms | 2.63 ms | |
| coadd 24 pairs @192² | 8.71 ms | 3.46 ms | |
| stack 60 frames @192² | 12.28 ms | 1.21 ms | ~10× |
| `/frames` raw 24 @120 cold | 92.3 ms | ~15–17 ms (p95 17.09) | zero-latency stub |
| `/frames` raw 24 @120 warm | — | 0.14 ms | response-cache hit |
| `/frames` diff 24 @192 warm | — | 0.06 ms | `perf_9` |
| 8× concurrent /frames @192 | wall 78 ms (1.2×) | wall 19 ms (3.6×) | distinct cold URIs |
| payload 24 @192 raw/gzip | 4.73 MB / 55 KB | unchanged | contract fixed |
| binary `spherex-backend` | 10,569,912 B | 8,059,064 B | strip+LTO |
| VmHWM, 3×60-frame requests | ~101 MiB peak | 153 MiB peak, +2 MiB/req | within 384 MiB budget |
