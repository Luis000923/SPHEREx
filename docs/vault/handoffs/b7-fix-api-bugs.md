# Handoff: b7-fix-api-bugs

Status: **done**. The 5 bugs found by the b6 battery are fixed and their
`#[ignore]`d tests now run and pass. Gates green on branch
`devin/b7-fix-api-bugs`. Not pushed.

## Scope note

The task's allowed scope listed `backend/src/api/`, `backend/src/error.rs`
and `backend/tests/*`. Two task items explicitly required edits elsewhere,
so they were applied with minimal, documented diffs:

- Bug 4 (log leaks) says "everywhere in `src/api` and `src/irsa`" →
  `src/irsa/error.rs` (`IrsaError::log_safe`) and `src/irsa/client.rs`
  (sanitised log sites, `transport_detail`).
- Bug 5 says "documenting the formula in `config.rs` docs" → doc-only
  change to `config.rs` (no behaviour change there).

`shared/openapi-contract.json`, `frontend/` and
`backend/src/api/targets.rs` untouched.

## Fixes (file, function, before/after)

### 1. `/api/v1/spectrum` did not range-check ra/dec — `backend/src/api/params.rs::spectrum_params`

Before: `coordinates()` parsed `ra`/`dec` but no range check ran, so
`ra=400`, `ra=-1`, `dec=±91`, `dec=inf` returned 404 `not_found`.
After: `validate_query_params(ra, dec, SPECTRUM_FOV_DEG)` (the fixed
cutout FOV the handler always uses; `/spectrum` has no `fov` parameter)
→ 400 `invalid_parameter`. `SPECTRUM_FOV_DEG` is now `pub(crate)` in
`src/api/spectrum.rs`. Test: `spectrum_range_checks_coordinates`.

### 2. `detector=N` empty set was 404 in diff/coadd/stack — `backend/src/api/frames.rs::compose`

Before: `raw` returned 200 + `frames: []`; `diff`/`coadd`/`stack` mapped
`ScienceError::EmptyInput` → 404 `not_found`.
After: `compose` returns `Ok(vec![])` early when `cutouts` is empty, so
all modes answer 200 + `frames: []` with correct `truncated` and
`total_observations` (the contract's 404 means "no observations at this
position", which is false here). Test:
`detector_filtered_empty_group_is_200_not_404`.

### 3. `/api/` paths outside `/api/v1` fell through to the SPA — `backend/src/api/mod.rs::build_router`

Before: `nest("/api/v1", ...)` matched `/api/v1` and `/api/v1/<non-empty>`
only; `/api/v2/frames`, `/api/`, `/api/v9`, `/api/v1/` were served
`index.html`.
After: the outer `fallback_service(spa)` is a handler that returns the
contract JSON `ErrorBody` 404 for `path == "/api"` or
`path.starts_with("/api/")` and delegates to `ServeDir`+`ServeFile` for
everything else (all methods; CORS/CSP/timeout layers still apply).
Test: `other_api_prefixes_also_return_json_404` (extended with bare
`/api` — the only input the ignored test did not already cover).

### 4. Logs leaked the full upstream URL — `src/api/fetch.rs`, `src/api/frames.rs`, `src/irsa/{error,client}.rs`

Root cause: `try_get` stored `format!("{kind}: {e}")` in
`Attempt::Retryable`, and reqwest error strings embed the full URL
("error sending request for url (…)"). That `detail` reached logs via
`warn!("cutout skipped: {e}")` and `debug!(... {detail} ...)`.

After:
- `client.rs::transport_detail` records only the error class
  (timeout / connect / body-read / request error) plus `e.url().host_str()`.
  Applied to send errors and body-read errors; `HTTP {status}` was
  already safe. This sanitises `IrsaError::Unavailable.detail` at the
  source (also what `debug!` prints and what the negative cache stores).
- New `IrsaError::log_safe()` (`irsa/error.rs`): kind + attempts +
  status + byte cap + sanitised detail; `Parse`/`Fits`/`Config` payloads
  never logged (Parse messages embed raw VOTable context).
- `api/fetch.rs`: `warn!("cutout skipped: {}", e.log_safe())`.
- `client.rs::fetch_frames_n`: `warn!("cutout for observation {id}
  skipped: {log_safe}")` — `id()` (FNV-1a hash of the access URL)
  replaces the URL itself.
- `client.rs`: `warn!("cache warm failed ... {}", e.log_safe())`,
  `warn!("S3 mirror cutout failed ({}) ...", e.log_safe())`.
- Kind-only labels for `JoinError` (`join_error_kind` in `fetch.rs` and
  `client.rs`), `ScienceError` (`science_error_kind` in `frames.rs`)
  and `FitsError` (`fits_error_kind` in `client.rs`, used by the two
  disk-cache `warn!`s).
- Left as-is (already safe, reviewed): disk-cache `std::io::Error`
  warns (OS error kind only, no path/URL), `warn!("background cutout
  fetches aborted ...")`, `warn!("ignoring invalid ALLOWED_ORIGINS
  entry")`, `warn!("frame dropped: wavelength unknown ...")`,
  `debug!("irsa search cache hit")`, counters in `debug!`/`info!`.

Test: `logs_do_not_leak_upstream_url_paths` — asserts the marker path
appears neither in the 502 body nor in captured tracing output.

### 5. Payload bound enforcement — `backend/src/api/frames.rs::payload_npix_cap` + `config.rs` docs

Before: `OUTPUT_NPIX=256` × `max_frames=60` produced **20,991,841 B
(~20.02 MiB)** — over the documented 20 MiB bound.
After: the effective raster side is
`min(output_npix(fov, cutouts, OUTPUT_NPIX), payload_npix_cap(n))` with
`n = cutouts.len()` (an upper bound on emitted frames for every mode).
Formula, also documented in `config.rs`:

  `n * (npix² * 4 B f32 * 4/3 base64 + FRAME_OVERHEAD_BYTES 2048) < 20 MiB`
  → `npix <= sqrt((20 MiB − n·2048) · 3 / (16·n))`

At `n=60` the cap is 255 px, so the production default (`OUTPUT_NPIX`
192) and the frontend's defaults (24 frames, FOV 0.05–0.2) are never
limited — it only binds for large `max_frames × output_npix` products.
Test: `largest_payload_at_max_npix_also_under_20mb` (added the same
`eprintln` measurement the sibling test prints).

## Measured payload numbers (this branch)

| Request | Before | After |
|---|---|---|
| `fov=0.5&max_frames=60`, `OUTPUT_NPIX=192` (default) | 11.27 MiB (11,817,721 B per b6) | **11.27 MiB (11,817,781 B)** — unchanged |
| same, `OUTPUT_NPIX=256` (legal ceiling) | ~20.0 MiB (20,991,841 B) — over bound | **19.86 MiB (20,829,181 B)**, raster 255×255 |

## Regression tests

The 5 un-ignored tests cover every exact input from the task; the only
addition was bare `/api` in `other_api_prefixes_also_return_json_404`
and the measurement `eprintln` in the max-npix test. No new test files.

## Open issues

None. Note: `/api` paths now return the JSON 404 for **all** methods
(previously non-GET methods on unknown paths also fell to the SPA);
the contract only specifies GET.

## Verify

`cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`

Results (2026-11-14, this branch): fmt clean; clippy clean; `cargo test`
→ **135 passed, 0 failed, 0 ignored** across all suites (security_tests
28/0, fuzz_tests 12/0, api_tests 25, irsa_tests 14, science 15,
validation 8, lib unit + fixtures 33).
