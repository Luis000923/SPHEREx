# Handoff: p5-perf-docs — document the performance work

Branch: `devin/p5-perf-docs`
Status: **done** — docs only; no code or `shared/` changes.

## Files changed

| File | Change |
|---|---|
| `README.md` | New "Performance (measured)" section between "Verified results" and "Limitations": backend and frontend tables plus technique bullets, every figure copied from `p1-backend-perf.md` / `p2-frontend-perf.md` |
| `docs/DEPLOY.md` | `MAX_CACHE_MB` row now documents the serialized `/api/v1/frames` response cache it also sizes (quarter of the value, clamped 4–64 MiB); two header bullets: the release profile (fat LTO, codegen-units 1, strip; measured 8.1 MB binary) and measured ~153 MiB peak RSS under three concurrent 60-frame requests |
| `docs/REFERENCES.md` | Added `rayon 1.12.0` (MIT OR Apache-2.0) plus its new transitive deps (`rayon-core 1.13.0`, `crossbeam-deque 0.8.8`, `either 1.19.0`) so the Rust crate list is again in sync with `backend/Cargo.lock` |
| `docs/vault/handoffs/p5-perf-docs.md` | this file |

## What was built

1. **README Performance section.** Covers, per the task list: release
   profile (fat LTO / cgu=1 / strip, binary 10.6 MB → 8.1 MB,
   `panic = "abort"` rejection rationale), reproject speedup
   (18.7 ms → 2.9 ms at 256², ~7×), response cache (warm hit ~0.06–0.14 ms,
   `MAX_CACHE_MB`/4 clamped 4–64 MiB, deadline-hit bodies never cached),
   concurrency scaling (8 concurrent requests: 78 ms/1.2× → 19 ms/3.6×),
   bundle sizes (first-load 86.4 → 82.4 kB gzip split into index +
   vendor-react + vendor-zod, ~1.15 MB of sourcemaps removed), decode time
   (59 ms → 11 ms for 24×192², sliced ≤ 6 ms), and playback re-renders
   (`history.replaceState` 47 → 2 in 1.5 s at 30 fps; zero React renders
   per frame). Every claim cites the handoffs; unmeasured "before" cells
   are marked `—` or described qualitatively, matching the sources.
2. **DEPLOY.md.** Only where perf work changed deployment semantics:
   `MAX_CACHE_MB` now feeds two caches (cutouts + serialized responses),
   and the release profile + measured RSS affect build/runtime sizing.
   No other rows touched — no setting name or default changed.
3. **REFERENCES.md.** `docs/submission/list-deps.sh` re-run for
   verification: the only missing Rust crates were rayon's dependency
   tree; all four entries added in sorted order with licenses from
   `cargo metadata`.

## Decisions / deviations

- **npm section of REFERENCES.md left as-is.** Regenerating also shows
  pre-existing drift unrelated to p1/p2 (Playwright/axe-core entries,
  hand-edited fontsource annotations). That drift predates the perf work
  and belongs to whoever owns the dep-list regeneration before
  submission, not to this task's "add rayon" scope. Flagged here for the
  PM.
- **Numbers rounded** in README (18.67 → 18.7 ms, 10,569,912 B → 10.6 MB)
  for readability; exact figures remain in the source handoffs, which the
  section links.
- "Within a 384 MiB limit" in DEPLOY.md mirrors p1's stated memory budget,
  not a new measurement.

## Open issues

- npm dependency list drift noted above (Playwright 1.65.0, axe-core
  4.14.0 MPL-2.0, pngjs missing from the rendered list; fontsource
  annotation lines differ from generator output). MPL-2.0 is not in the
  post-list license notes either — worth a sweep before submission.
- `/spectrum` still reprojects on the async executor path (carried over
  from p1 open issues; not a docs matter).

## Verification

```
# diff of generated vs documented crate list — only rayon tree differed,
# now clean:
docs/submission/list-deps.sh | sed -n '/^### Rust crates/,/^### npm/p' | sed '$d' > /tmp/crates-new.txt
sed -n '/^### Rust crates/,/^Backend crates whose/p' docs/REFERENCES.md | sed '$d' > /tmp/crates-old.txt
diff /tmp/crates-old.txt /tmp/crates-new.txt   # empty — in sync
```

All README figures cross-checked line-by-line against the "Before / after"
tables of `p1-backend-perf.md` (lines 87–104) and `p2-frontend-perf.md`
(lines 67–89). Docs only: no code gates applicable; `git diff --stat`
confirms no file outside the allowed scope changed.
