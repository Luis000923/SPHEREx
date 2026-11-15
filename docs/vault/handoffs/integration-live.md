# Handoff: integration-live (real SPHEREx data end to end)

Status: **backend done, verified live; frontend changes done in commit
`765a03d` but frontend is frozen (teammate owns it) — see "Notes for the
frontend teammate"**. Browser check (step 6) NOT done: new rule forbids
vite/playwright servers and frontend edits; no screenshots taken.
Agent: Claude Opus 5.5. Branch `opus/integration-live`. Not pushed.

## Commits (on top of `ea20f6d`)

| Commit | What |
|---|---|
| `5af2da4` | request-wide deadline, soft deadline, background completion, negative cache, coalescing, adaptive grid, spectrum cap, warm selection |
| `121f8d7` | idle-read timeout, optional `CACHE_DIR` disk cache without EPSF |
| `e1680ec` | cutouts from the S3 mirror via byte ranges (IBE fallback); north-up grid |
| `765a03d` | frontend: TAN click->sky, `det` URL key, follow-up polling, default `max_frames` (+ concurrent SIA collections in backend) |
| `905869a` | main's b6 security/fuzz tests brought in and adapted |
| `4ee9a34`+ | `scripts/warm.sh`, `SI_USAGE.md` row, this handoff |

## Files

Backend: `src/api/{fetch,frames,spectrum,mod,params}.rs`, `src/config.rs`,
`src/main.rs`, `src/irsa/{client,config,mod}.rs`, `src/fits/{mod,hdu}.rs`,
new `src/fits/subimage.rs`, `src/science/grid.rs`, `Cargo.toml`
(tokio `fs`; dev-dep `futures-util`), tests `api_tests.rs`,
`irsa_tests.rs`, `fits_tests.rs`, `security_tests.rs`, `support/mod.rs`,
`fuzz_tests.rs` (last three from main). Frontend (commit `765a03d` only):
`src/components/spectrum/pick.ts`, `src/hooks/useFrames.ts`, new
`src/lib/poll.ts`, `src/lib/urlState.ts`, `src/App.tsx`,
`src/components/viewer/SkyExplorer.tsx`, tests `tests/spectrum/pick.test.ts`,
`tests/viewer/{poll,urlState}.test.ts`, `tests/integration/useFrames-poll.test.tsx`.
Other: `scripts/warm.sh`, `SI_USAGE.md`.

## Bugs found live and fixed (each with a regression test that failed first)

1. **Deadline ignored the SIA search** -> cold requests got a bare 408 after
   55 s. Search now runs under the same deadline (detached, keeps filling
   the cache). Test `frames_deadline_includes_search`.
2. **In-flight cutouts were aborted at the deadline**, wasting upstream work;
   Retry started from zero. They now finish in the background (grace
   600 s, then aborted). Tests `deadline_lets_in_flight_cutouts_finish`,
   adapted b6 `deadline_*`.
3. **Failed cutouts were refetched on every request** (a "cached" request
   took ~32 s retrying one bad observation). Negative cache: 4xx (e.g. 422)
   1 h, transient (503 after retries, timeouts) `IRSA_FAILURE_TTL_SECS`=60.
   Test `failed_cutout_is_not_refetched_immediately`.
4. **No coalescing** of identical concurrent fetches -> moka `try_get_with`.
   Test `concurrent_identical_cutouts_are_coalesced`.
5. **Total request timeout cut every real cutout**: IBE streamed 3.96 MB at
   ~55 KB/s (73 s); a 15/30 s total timeout killed each mid-body ("error
   decoding response body"), so nothing ever decoded. Now connect +
   idle-read timeout (`IRSA_TIMEOUT_SECS`=30) plus total cap
   `IRSA_TOTAL_TIMEOUT_SECS`=300. Test `slow_but_steady_body_is_not_cut_by_timeout`.
6. **Orientation: images were north DOWN (mirror image)**. Output grid had
   CD2_2 > 0 (row 0 = south) and the viewer draws row 0 at the top. Grid is
   now `OutputGrid::centered_north_first` (CD2_2 < 0, row 0 = north, col 0 =
   east). WCS in the response stays consistent with the pixel order, so the
   frontend needs no change. Test `frames_rows_are_north_up_east_left`;
   confirmed live by Barnard's Star moving towards row 0 (north).
7. **Output grid always 192 px** whatever the FOV (6.6x oversampling of a
   29 px cutout). Now 4 samples per native pixel (scale read from the frame
   WCS), rounded up to a multiple of 8, capped by `OUTPUT_NPIX`. 0.05 deg ->
   120 px. Test `frames_grid_follows_native_pixel_scale`.
8. **Soft deadline**: `REQUEST_SOFT_DEADLINE_SECS`=8 — answer once >= 2 frames
   decoded, keep downloading. Test `soft_deadline_returns_cached_frames_early`.
9. Spectrum fetched up to 60 cutouts per click -> 24 (`SPECTRUM_MAX_FRAMES`).
10. `WARM_TARGETS` warmed a 60-frame selection, disjoint from the 24 the API
    selects -> `warm_with(.., DEFAULT_MAX_FRAMES)`.
11. SIA collections queried sequentially (20-33 s each near Barnard) -> concurrent.
12. Frontend (commit `765a03d`, frozen now): `pixelToSky` was linear without
    1/cos(dec) — clicks 2.5x off in RA at R Draconis; exact TAN inverse.

## S3 byte-range cutouts (design and why)

Measured 2026-11-14/15 from this machine: IBE cutout = 3.96 MB per 0.05 deg
cutout, **97% the unchanged EPSF table** (3.86 MB), served at ~55 KB/s
(73 s), frequent 503 and connections dropped at ~50 s; `gzip=true` only
shrinks it to 3.73 MB; IBE has no HDU selection. The same L2 files are in
the public AWS Open Data bucket `nasa-irsa-spherex` (SIA `cloud_access`
column; object key = path after `/ibe/data/spherex/`), Range supported,
~7 MB/s measured minutes after the 55 KB/s IBE measurement.

`fits::subimage` + `IrsaClient::cutout_from_s3` rebuild exactly the IBE
cutout: read head (PRIMARY+IMAGE header) and file tail (WCS-WAVE, suffix
range) together; compute the box from the TAN+SIP WCS; download only the
box rows of IMAGE/VARIANCE/ZODI; for FLAGS (RICE_1, one tile per row in the
parent) download only those table rows + their heap span and decode them;
shift `NAXISn`, `CRPIXn`, `CRPIXnA`, `CRPIXnW` exactly as IBE does (verified
key by key); never read EPSF. **Verified bit-identical** (IMAGE, VARIANCE,
ZODI, FLAGS, CRPIX, legend, wavelength) against a real IBE cutout:
`live_s3_cutout_matches_ibe_reference` (opt-in). Synthetic test
`s3_range_cutout_matches_subarray` (stub with Range support; asserts no IBE
hit, < half the file downloaded, 422 outside the array).
Config: `CUTOUT_SOURCE=s3` (default) | `ibe`; `SPHEREX_S3_BASE_URL`;
`S3_MAX_CONCURRENCY`=16 (separate semaphore: AWS is not the shared IRSA
service; IRSA SIA and IBE stay at `IRSA_MAX_CONCURRENCY`=3). Any mirror
error except 422 falls back to IBE (covered by existing stub tests whose
URLs are not mirror paths; no live fallback event occurred: 0 in logs).
Per 0.05 deg frame ~1 MB is downloaded (full-width rows), stored on disk
EPSF-free (~100-150 KB; 63 cutouts = 9.4 MB).

`CACHE_DIR` (optional): SIA bodies (24 h TTL) and cutouts survive restarts.
Recommended for deploy, plus `scripts/warm.sh` after start.

## Measured numbers (live, 2026-11-14/15; network of this machine was shared and varied 1 KB/s - 7 MB/s, treat timings as indicative)

| Measurement | Value |
|---|---|
| IBE cutout 0.05 deg (isolated curl) | 3 962 880 B, 73.3 s, 54 KB/s; many 503 |
| S3 cutout same file (live test) | 9.8 s, bit-identical to IBE |
| WISE 0855 `/frames` raw, cold (S3, search disk-cached) | **7.6 s, 24/24 frames** |
| WISE 0855 raw, cached | **0.20-0.23 s** |
| WISE 0855 diff, cached | 0.17 s, 5 frames |
| WISE raw payload | 1.85 MB JSON, 1.09 MB gzip (120x120 px x 24) |
| Barnard (propagated pos, 6 frames) | 1st 502 at 40 s (SIA search slow), then 8.0 s x2 partial (2, 5), then 0.05 s with 6/6 |
| Spectrum at Barnard, cold | 502 (SIA 502 outage) then 200 in 28.9 s with 2 points (soft deadline) |
| Spectrum cached | 1 ms, 24 points |
| SIA latency | 1.5-2.5 s at WISE; 19-33 s per collection near Barnard; HTTP 502 outage ~02:52-03:03Z (confirmed with direct curl), recovered intermittently |
| Before fixes | cold: 408 at 55 s; "cached": 32-40 s; after restart: 0 frames for >10 min |

WISE 0855 (`ra=08h55m10.831s dec=-07d14m42.53s fov=0.05`): total_observations
271, 24 selected (spread MJD 60800.7-61174.3, i.e. 1.02 yr), truncated=true
(cap). Per detector D1 5, D2 4, D3 5, D4 3, D5 3, D6 4; wavelengths 0.84-4.93 um,
one per frame (WCS-WAVE at the centre). Diff: 5 frames (only 5 detector x
wavelength groups with >= 2 epochs within 0.05 um), so diff mode is sparse
with 24 frames.
**WCS alignment**: brightest field star (stack S/N 16.6) found in 17/24
frames; centroid scatter **0.24" (x) / 0.26" (y) rms, max 0.63"** (output
pixel 1.5", native ~6.15"). Second star: 0.38"/0.29" rms in 12 frames.
Orientation confirmed by Barnard's motion (below); independent catalog
check (Gaia via TAP/VizieR) failed with network errors — not done.

## Science sanity (proper motion)

- **Barnard's Star**: detected as the brightest source (peak 630-1735 MJy/sr,
  flags include OVERFLOW/BLOOM: saturated core) at the position propagated to
  ~2025.9 (269.4464, +4.7667), within ~2" of it. Over 1.10 yr (6 epochs) the
  fitted motion is **+11.1"/yr north, 0.0"/yr east** (expected +10.36 N,
  -0.80 E), residual rms 2.1" N / 1.3" E, i.e. ~12" ≈ 2 native pixels total:
  **detectable** with centroiding, not by eye frame to frame.
- **WISE 0855**: expected ~8.1"/yr x 1.02 yr ≈ 8" ≈ 1.3 native px over the
  available epochs. At the predicted position (assuming the official
  coordinates are the AllWISE ~2010.5 epoch: offset -121" RA*, +10" Dec) an
  aperture test gives +3.9 sigma at 4.42 um (D6) but also +3.2 sigma at 2.36 um
  where a ~250 K Y dwarf should be invisible, and < 1 sigma elsewhere:
  **not a convincing detection; its motion is not measurable here**. Said
  honestly in any demo.

## Decisions for the orchestrator

1. **Presets do not contain 2-3 of the targets at SPHEREx epochs** (high
   proper motion; values from memory except Barnard, which the data confirm):
   - Barnard's Star preset is the J2000 epoch-2000 position; the star is now
     ~263" north, outside the 0.1 deg FOV (±180"). Measured: use
     **ra 269.4464, dec +4.7667** (propagated to ~2025.9), fov 0.05-0.1.
   - WISE 0855 at the official coordinates lies ~121-129" west (outside the
     ±90" of fov 0.05). Options: keep official coords with fov 0.1, or centre
     on 133.7611, -7.2422 (needs confirming the epoch of the official coords).
   - Epsilon Indi B (pm ~4.7"/yr): likely ~118" off, outside fov 0.05.
     UNVERIFIED. RW Aur and R Dra have negligible proper motion.
   Change needs `backend/src/api/targets.rs` AND `frontend/src/lib/presets.ts`
   together (the api test asserts the same values); I did not change them.
2. **Merge the S3 path?** It is what makes the app usable (7.6 s cold vs
   minutes or never); default on, `CUTOUT_SOURCE=ibe` reverts. Cite AWS Open
   Data / `nasa-irsa-spherex` in `docs/REFERENCES.md` (not edited by me).
3. Deploy with `CACHE_DIR` on a persistent volume and run `scripts/warm.sh`
   after start (not run end to end here: SIA outage and slow shared network;
   syntax-checked only). Expect ~10 s cold per preset on a datacenter link.
4. Contract: no change required. Proposal only: document that `data_b64`
   row 0 is north (top) and that `truncated` is also true when capped.

## Notes for the frontend teammate (commit `765a03d` = patch to apply)

- **Polling contract**: `/frames` answers at the soft deadline (8 s) with
  >= 2 decoded frames and keeps downloading; a 502 `upstream_unavailable`
  means nothing decoded yet but the fetch continues. Re-request the same
  URL: `lib/poll.ts` re-asks every 6 s (max 10) while a raw series has
  fewer than min(24, total_observations) frames, retries 502 only, never
  404/400; other modes poll while the frame count grows. `truncated` is
  true when merely capped (271 > 24), so it cannot be the stop signal.
- Omit `max_frames` (contract default 24 = the warmed selection); 60 costs
  2.5x and misses the warm cache.
- `pick.ts`: use the TAN inverse (in the patch); the linear form is wrong
  away from the equator.
- `det` URL key 1-6 for the detector filter (in the patch).
- Orientation needs nothing: backend now sends row 0 = north with a WCS
  whose CD2_2 < 0; `gl.ts` row-0-at-top is correct.
- UX text added in the patch (English): "Fetching SPHEREx cutouts from
  IRSA…", "Loading more frames… N so far.", busy-archive error message.

## Not verified / open

- No browser check, no screenshots (frontend frozen by new rule).
- Live IBE fallback after an S3 failure never triggered live (tested only
  via stubs/config). WARM_TARGETS + warm.sh not run end to end.
- Eps Indi B / WISE epoch of official coordinates not verified.
- b6 known bugs untouched (5 tests still `#[ignore]`).
- `diff` mode yields few frames with 24 spread epochs (needs same detector
  and wavelength within 0.05 um); product decision whether to select
  epochs per detector for diff.

## Verify

```
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
# ok: 122 passed, 5 ignored (b6 known bugs), 0 failed
SPHEREX_LIVE=1 SPHEREX_IBE_REF=<ibe cutout.fits> SPHEREX_REF_URL=<access_url> \
  SPHEREX_REF_POS=133.79512916666667,-7.245147222222222,0.05 \
  cargo test --test irsa_tests live_s3 -- --nocapture   # ok, 9.8 s, bit-identical
cd frontend && npx tsc --noEmit && npx vitest run && npx eslint .   # at 765a03d: ok, 113 tests
# vite build last run at start of task (ok); not re-run after the frontend freeze
bash -n scripts/warm.sh   # ok
```
