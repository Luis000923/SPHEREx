# Use of Super Intelligence (SI/AI) tools

Rule: SI may accelerate development but must not be used to come up with the idea of how to solve the challenge. All SI use is disclosed here.

TODO(team): state accurately who defined the product idea. In this project Claude proposed the problem/solution definition (`docs/problem-solution.md`, decision D-001) from the challenge text and the user approved it; see `docs/vault/open-questions.md`. Do not submit this wording as is.

| Area | Tool | How it was used | Human review |
|---|---|---|---|
| Coordination, architecture review, OpenAPI contract, task breakdown, vault, scripts, project notes | Claude (Claude Code) | Proposed the problem/solution definition and plan from the challenge text (approved by the user), drafted the API contract, backlog, helper scripts and reviews; merged approved work. Wrote no application feature code. | Team |
| Rust backend modules, TypeScript frontend components, Dockerfile/CI, tests, documentation (incl. submission drafts) | Devin | Implemented tasks in `docs/backlog.md` in isolated branches; commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |
| Live-data integration and numeric verification (task `integration-live`) | Claude Opus 5.5 agent | Cross-cutting integration of backend and frontend with real data; commits carry `Generated-by: Claude Opus 5.5 (SI-assisted)` | Claude + team review before merge |
| Visual design pass (tokens, layout, CSS, layout tests) | Claude Opus 5.5 (agent spherex-frontend-designer) | Task `f6-visual-polish`: design tokens, docked spectrum layout, component CSS, layout and contrast tests from the team's UI rules; commits carry `Generated-by: Claude Opus 5.5 (SI-assisted)` | Claude + team review before merge |
| Backend unification (task `b12-backend-unify`) | Devin | Composed one backend branch from `opus/integration-live`, `b7`, `b11`, `b8`, `b9` path-wise (frontend untouched), adapted the perf budgets to the native-scale grid, documented the new env vars; commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |
| Checklist backend tasks (b13 diff selection, b14 spectrum latency, b15 SIA resilience/TAP fallback, b16 docs/deploy/deny.toml, b18 spectrum cache reuse) | Devin | Implemented the checklist tasks in stacked branches; commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |
| Final backend unification (task `b19-backend-final-unify`) | Devin | Composed b13/b14/b15/b16/b18 onto b12 path-wise (frontend untouched), resolved the b15-vs-b18 overlaps, applied orchestrator decisions (HEALTH_PROBE_SECS=300, contract Health optional fields, env doc sync, TAP references); commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |
| Baked preset cache + offline proof (task `b17-bake-verify`) | Devin | `scripts/bake-cache.sh`, `scripts/probe-presets.sh`, `docker-entrypoint.sh`, Dockerfile/.dockerignore/DEPLOY.md edits, `docs/live-results/*.json`; verified `docker run --network none`; commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |
| Real-backend frontend integration (task `f9-real-data-integration`) | Claude Opus 5.5 agent | Polling/partial states, TAN click→sky, `det` URL key, epoch-2026.0 presets, honest progress UI; commits carry `Generated-by: Claude Opus 5.5 (SI-assisted)` | Claude + team review before merge |
| End-to-end real-data QA (task `f10-real-data-qa`) | Claude Opus 5.5 agent | Drove the real product in Chrome against the final backend + baked cache; found and fixed 7 frontend defects with regression tests; produced `docs/screens-real/` and the measured numbers used in the submission texts; commits carry `Generated-by: Claude Opus 5.5 (SI-assisted)` | Claude + team review before merge |
| Preset coordinate snap + serve-stale (task `b20-preset-snap-stale`) | Devin | Snap rounded/re-typed preset coordinates before cache lookup, serve-stale for aged search bodies, `SEARCH_DISK_TTL_SECS`; commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |
| Final submission texts (task `submission-final`) | Devin | Rewrote README, project page, checklist, demo outline/script and this file with only measured, verified facts; commits carry `Generated-by: Devin (SI-assisted)` | Claude + team review before merge |

SI-generated: essentially all application code, tests, Dockerfile/CI and
documentation text. Not SI-generated: reported measurements and any
screenshots/recordings in the demo (real output of the running application).
TODO(team): authorship of the idea and product definition (see above).

No SI-generated images, audio or video are used. No NASA logos or branding
are generated.
