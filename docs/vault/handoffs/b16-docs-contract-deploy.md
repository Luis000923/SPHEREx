# Handoff: b16-docs-contract-deploy

Status: **done**. Branch `devin/b16-docs-contract-deploy`, not pushed.
Agent: Devin. Date: 2026-11-15.

## What was built

Backend-side housekeeping: contract text clarifications, env-var docs kept
in sync with the code, deploy docs for the cache volume and reverse-proxy
rate limiting, supply-chain references and `cargo deny` enforcement.

## Files

- `shared/openapi-contract.json` — three `description` strings only:
  - `Frame.data_b64`: row 0 is the NORTH edge of the cutout (north up,
    east left); WCS consistent with that pixel order.
  - `FramesResponse.truncated`: true also when the `max_frames` cap alone
    limited the result, or a deadline stopped the fetch early.
  - `GET /api/v1/frames`: a cold request may return partial results once
    enough frames decoded (soft deadline) while the rest download in the
    background; repeating the identical request completes the set.
- `docs/DEPLOY.md` — `CACHE_DIR` row points at a new "Cache volume"
  section (uid 999 write requirements, named volume vs `install -d -o
  999` bind mount); new "Behind a reverse proxy" section (rate limiter
  keys on `X-Forwarded-For`, correct behind Caddy/nginx, evadable if the
  port is exposed — publish on `127.0.0.1` or firewall; `ALLOWED_ORIGINS`
  only when the frontend lives on another origin); CI section documents
  the new `supply-chain` job.
- `scripts/container.env.example` — added the missing `STATIC_DIR`
  (default `/app/static`).
- `docker-compose.yml` — `CACHE_DIR` named-volume comment documenting the
  uid-999 write requirement, the named-volume approach (recommended) and
  the `install -d -o 999 -g 999` bind-mount alternative.
- `docs/REFERENCES.md` — AWS Open Data bucket `nasa-irsa-spherex`
  (cited via https://irsa.ipac.caltech.edu/cloud_access/ and
  https://registry.opendata.aws/spherex-qr/); new license-notes block
  listing every backend crate whose license field lacks MIT/Apache-2.0
  (ISC, BSD-3-Clause, Unicode-3.0 ICU4X suite, Zlib, CDLA-Permissive-2.0)
  and noting `r-efi` is used under its MIT OR Apache-2.0 option (not LGPL).
- `backend/deny.toml` — new. License allow-list (MIT, Apache-2.0,
  Apache-2.0 WITH LLVM-exception, BSD-2/3, ISC, Unicode-3.0, Zlib,
  MPL-2.0, CC0-1.0, Unlicense, BSL-1.0, CDLA-Permissive-2.0); advisories
  (vulnerabilities error by default in cargo-deny 0.20; unmaintained and
  unsound scope `all`; yanked warns); bans (`multiple-versions`,
  `wildcards` warn); sources restricted to crates.io.
- `.github/workflows/ci.yml` — new `supply-chain` job using the pinned
  major tag `EmbarkStudios/cargo-deny-action@v2`
  (`manifest-path: backend/Cargo.toml`,
  `command: check licenses advisories bans`). No secrets needed; the
  action fetches the public RustSec DB over https.

## Env-var sync (re-extracted from the code)

Grepped `backend/src/config.rs` and `backend/src/irsa/config.rs`: 24
variables (`PORT STATIC_DIR ALLOWED_ORIGINS RATE_LIMIT_RPS OUTPUT_NPIX
REQUEST_DEADLINE_SECS REQUEST_SOFT_DEADLINE_SECS WARM_TARGETS
IRSA_BASE_URL IRSA_COLLECTIONS IRSA_MAX_CONCURRENCY IRSA_TIMEOUT_SECS
IRSA_TOTAL_TIMEOUT_SECS IRSA_FAILURE_TTL_SECS IRSA_MAX_ATTEMPTS
IRSA_RETRY_BASE_MS IRSA_MAX_BODY_MB CUTOUT_SOURCE SPHEREX_S3_BASE_URL
S3_MAX_CONCURRENCY CACHE_DIR MAX_FRAMES MAX_CACHE_MB`; plus `RUST_LOG`
read by `main.rs`'s `EnvFilter`). All now appear in `docs/DEPLOY.md`,
`docker-compose.yml` and `scripts/container.env.example`; `STATIC_DIR`
was the only gap (missing from the env example).

## Verify commands and results

### jq proof: only descriptions changed

```sh
git show HEAD:shared/openapi-contract.json > /tmp/contract-before.json
jq 'del(..|.description?)' /tmp/contract-before.json > /tmp/contract-before-nodesc.json
jq 'del(..|.description?)' shared/openapi-contract.json > /tmp/contract-after-nodesc.json
diff /tmp/contract-before-nodesc.json /tmp/contract-after-nodesc.json
# -> no output; files identical with all descriptions removed.
```

`git diff --stat shared/openapi-contract.json`: 3 insertions, 3 deletions
(the three description strings). `jq empty` validates the JSON.

### cargo deny

```sh
cd backend && cargo deny check licenses advisories bans
# advisories ok, bans ok, licenses ok  (exit 0)
# warnings only: duplicate crate versions (base64, getrandom, hashbrown,
# syn, windows-sys) — normal for a Rust tree, warn by policy.
```

cargo-deny 0.20.2 local. Advisory check fetched the RustSec DB.

### Docker volume permission test (real run, fresh named volume)

Image built from this worktree (`docker build -q -t spherex-blink:b16 .`):

```sh
docker volume create b16-cache-test
docker run --rm -v b16-cache-test:/data/cache -e CACHE_DIR=/data/cache \
  spherex-blink:b16 sh -c 'id; touch /data/cache/probe && ls -la /data/cache && echo WRITE-OK'
```

Output:

```
uid=999(app) gid=999(app) groups=999(app)
drwxr-xr-x 2 app  app  4096 Nov 15 03:44 .
drwxr-xr-x 3 root root 4096 Nov 15 03:34 ..
-rw-r--r-- 1 app  app     0 Nov 15 03:44 probe
WRITE-OK
```

End-to-end sanity on the same volume:

```sh
docker run -d --name b16-cache-run -v b16-cache-test:/data/cache \
  -e CACHE_DIR=/data/cache -p 127.0.0.1:18099:8080 spherex-blink:b16
curl -fsS http://127.0.0.1:18099/api/v1/health
# {"status":"ok","version":"0.1.0","collections":["spherex_dr1","spherex_qr3"]}
```

Volume, container and `spherex-blink:b16` scratch artifacts cleaned up.

### YAML/JSON validity

- `python3 -c "yaml.safe_load(open('.github/workflows/ci.yml'))"` -> OK.
- `docker compose config -q` -> clean (exit 0).
- `jq empty shared/openapi-contract.json` -> OK.

## Decisions / deviations

- `deny.toml` tuned for cargo-deny 0.20: `vulnerability`/`notice` keys are
  removed upstream (all vulnerability advisories error by default);
  `unmaintained`/`unsound` use scope values (`all`), not warn/deny.
  `unused-allowed-license = "allow"` keeps the mandated allow-list entries
  (MPL-2.0, CC0-1.0) warning-free while unused.
- Compose ports stay `${PORT:-8080}:${PORT:-8080}` (unchanged); the
  localhost-bind guidance lives in DEPLOY.md for direct `docker run` use —
  compose is the operator's choice to publish more widely.
- REFERENCES.md notes list every crate without MIT/Apache in its license
  field, taken from `cargo metadata` (not only `cargo deny list`), plus
  the explicit r-efi and webpki-roots clarifications the task asked for.

## Open issues

- None blocking. Advisory check needs network (normal); the GitHub action
  has it. If a future advisory hits the tree, add a reviewed entry to
  `advisories.ignore` in `backend/deny.toml`.
