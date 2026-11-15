# Deploying SPHEREx Blink

The whole app ships as a single container image: the Rust binary serves the
REST API and the built frontend from `STATIC_DIR`. The image is
hosting-neutral — it works on Render, Fly.io, Railway, Azure Container Apps,
or a plain VPS with Docker.

- Build context: repository root.
- Runtime base: `debian:bookworm-slim` + `ca-certificates` + `curl`
  (HEALTHCHECK only), non-root user `app` (uid 999).
- Measured image size: **177.9 MiB** (186,536,869 bytes) with the baked
  preset cache (~30 MB on disk), **139.5 MiB** (146,257,088 bytes) without it
  (`docker image inspect --format '{{.Size}}'`, 2026-11-15).
- Measured build time: **36 s** clean (`docker build --no-cache`),
  **3 s** fully cached, on a 16-thread i7-13620H with the base images
  already pulled (2026-11-15).
- Exposed port: `8080` (override with `PORT`). Health endpoint:
  `GET /api/v1/health`.

## Build and run locally

```sh
docker build -t spherex-blink .
docker run --rm -p 8080:8080 -e WARM_TARGETS=0 spherex-blink
# open http://localhost:8080
```

`WARM_TARGETS=0` only skips the startup cache warm-up, which would fetch
the five presets from IRSA; drop it in production if you want the warm-up.

Or with Compose (copy `scripts/container.env.example` to `.env` first to
change defaults — Compose interpolates `.env` automatically):

```sh
cp scripts/container.env.example .env   # optional, sane defaults work as-is
docker compose up --build
```

`scripts/docker-smoke.sh` builds the image if it is missing, starts a
container on a random localhost port and verifies the whole serving surface:
API JSON routes, the five presets, SPA serving and fallback, gzip on
`/assets`, `/?mock=1`, security headers, JSON error bodies, non-root user,
TLS to IRSA, the Docker healthcheck and graceful SIGTERM shutdown. It exits
non-zero on the first failed check and always removes the container.

## Baked preset cache

`scripts/bake-cache.sh` downloads the data of the five official presets once
(SIA result bodies plus EPSF-stripped cutouts for the default raw and diff
selections, and whatever `/api/v1/spectrum` needs at each target position)
into `cache-bake/` at the repo root:

```sh
scripts/bake-cache.sh          # ~10-40 min depending on IRSA load
docker build -t spherex-blink .
```

The Dockerfile copies `cache-bake/` into the image at `/data/cache` (owned
by uid 999) and sets `CACHE_DIR=/data/cache` by default, so the shipped
image answers all five presets — `/api/v1/frames` raw and diff and
`/api/v1/spectrum` at the target positions — with **zero upstream traffic**.
Verified with `docker run --network none`; a container with no network at
all still serves the presets. IRSA stays a live fallback for any other
position.

`cache-bake/` is not committed to git (see the b17 handoff for the
commit-vs-CI decision). A build without the directory still works — the
image simply starts with an empty disk cache.

A mounted volume still works and is additive: a named volume is initialized
from the image's `/data/cache` on first use (it inherits the baked data),
and a bind mount replaces it entirely (cold cache, still functional).

Freshness and serve-stale: the backend treats cached SIA bodies as fresh
for `SEARCH_DISK_TTL_SECS` (default 86400 s) counted from file mtime;
cutouts never expire. A stale body is refetched upstream — fresh data
wins whenever IRSA answers — and a transient refetch failure (5xx,
timeouts, transport errors) serves the stale body instead of erroring,
so the baked presets keep working while upstream is unreachable. The
image entrypoint (`scripts/docker-entrypoint.sh`) still refreshes mtimes
at start and hourly as belt and braces: serve-stale makes it optional.

Coordinate snapping: `/api/v1/frames` positions within
`PRESET_SNAP_ARCSEC` (0.3 arcsec, a constant in
`backend/src/api/params.rs` covering every 4-decimal-place re-typing)
of a preset target — with `fov` within 1e-3 deg of that preset's — are
rewritten to the preset's exact constants before any cache lookup, and
the response `query` echo reports the snapped values. This keeps the
baked cache valid for rounded or re-typed coordinates (e.g.
`ra=269.4463` for Barnard's Star); positions beyond the snap radius or
with a different `fov` are unaffected.

## Environment variables

All settings are plain configuration; the app needs **no secrets**. Every
variable below is read by `backend/src/config.rs` or
`backend/src/irsa/config.rs`; the table is kept in sync with those files.

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | TCP port the server listens on. |
| `STATIC_DIR` | `frontend/dist` | Directory with the built frontend (SPA fallback to `index.html`). The image sets `/app/static`. |
| `RUST_LOG` | `info` | `tracing-subscriber` env filter. |
| `ALLOWED_ORIGINS` | empty | Comma-separated CORS origins. Empty is strictest: no cross-origin requests. The UI is served same-origin, so CORS is not needed unless the API is called from another site. |
| `RATE_LIMIT_RPS` | `4` | Per-client API GET rate limit, requests/second (also the burst size). |
| `OUTPUT_NPIX` | `192` | Cap on the output raster side for `/api/v1/frames`. The API picks the actual side from the cutout plate scale (~4 samples per detector pixel, e.g. 120 px at fov 0.05 deg), clamped to this cap (contract max 256). |
| `REQUEST_DEADLINE_SECS` | `40` | Total upstream-fetch budget per API request; on expiry the API returns partial frames with `truncated=true`. |
| `REQUEST_SOFT_DEADLINE_SECS` | `8` | Answer early once at least 2 frames have decoded; the request keeps filling the cache in the background. |
| `WARM_TARGETS` | unset | Set to `1` (or `true`/`yes`/`on`) to pre-fetch the five preset targets through the cache at startup. Boolean flag, not a target list. |
| `IRSA_BASE_URL` | `https://irsa.ipac.caltech.edu` | NASA/IPAC IRSA service root. |
| `IRSA_COLLECTIONS` | `spherex_dr1,spherex_qr3` | Comma-separated collections, in query order. |
| `IRSA_MAX_CONCURRENCY` | `3` | Max parallel outbound IRSA requests. Keep ≤ 3; IRSA is a shared public service. |
| `IRSA_TIMEOUT_SECS` | `30` | Connect/idle-read timeout for a single IRSA request, seconds. |
| `IRSA_TOTAL_TIMEOUT_SECS` | `300` | Cap on one whole upstream transfer (headers + body), seconds. |
| `IRSA_FAILURE_TTL_SECS` | `60` | How long a transient cutout/search failure is remembered before retrying upstream (negative cache), seconds. |
| `IRSA_MAX_ATTEMPTS` | `3` | Attempts per IRSA request, with exponential backoff on 503/429. |
| `IRSA_RETRY_BASE_MS` | `400` | Base backoff between IRSA retries, milliseconds. |
| `IRSA_MAX_BODY_MB` | `64` | Maximum accepted IRSA response body, MiB. |
| `SIA_FALLBACK_TAP` | `1` | Query the IRSA TAP service when every SIA attempt fails transiently (503/429/timeout/5xx after all retries). Accepts `1`/`0`, `true`/`false`, `yes`/`no`, `on`/`off`. |
| `IRSA_TAP_URL` | `https://irsa.ipac.caltech.edu/TAP` | IVOA TAP endpoint base used by the SIA fallback; `/sync` is appended. Must be non-empty while `SIA_FALLBACK_TAP` is on. |
| `HEALTH_PROBE_SECS` | `300` | Minimum spacing between the detached upstream health probes `/api/v1/health` may spawn when no upstream contact finished recently. Real traffic resets the timer, so a busy app never probes; the container HEALTHCHECK polls `/health` every 30 s, so the default keeps idle probes to once per 5 minutes. |
| `CUTOUT_SOURCE` | `s3` | Cutout source: `s3` assembles cutouts from byte ranges of the AWS mirror (IBE fallback); `ibe` uses the IRSA IBE cutout service only. |
| `SPHEREX_S3_BASE_URL` | `https://nasa-irsa-spherex.s3.us-east-1.amazonaws.com` | Base URL of the AWS S3 mirror of the `/ibe/data/spherex/` paths. |
| `S3_MAX_CONCURRENCY` | `16` | Parallel byte-range requests to the S3 mirror; separate from `IRSA_MAX_CONCURRENCY` (AWS is not the shared IRSA service). |
| `CACHE_DIR` | `/data/cache` in the image (unset in source = memory only) | Persistent cache directory: SIA results and cutouts survive restarts. The image bakes the five presets here (see "Baked preset cache"). Compose mounts the `spherex-cache` volume at `/data/cache`; the container user is uid 999 and must be able to write it (see "Cache volume" below). |
| `MAX_FRAMES` | `60` | Upper bound on frames per `/api/v1/frames` (contract max 60). |
| `MAX_CACHE_MB` | `200` | In-memory cutout cache cap, MiB. |
| `SEARCH_DISK_TTL_SECS` | `86400` | Freshness window for on-disk SIA/TAP search bodies, seconds (counted from file mtime). Older bodies are refetched upstream; a transient refetch failure serves the stale body (keeps the baked cache answering while IRSA is unreachable). `0` = always try upstream first. |

`PORT`, `STATIC_DIR`, `RUST_LOG`, `MAX_CACHE_MB`, `IRSA_MAX_CONCURRENCY` and
`CACHE_DIR` have defaults baked into the image; set the rest per environment
as needed.

## Cache volume

The image creates `/data/cache` owned by `app:app` (uid/gid 999, the
non-root `USER` the container runs as). A **named volume** (the Compose
default) is initialized from the image contents on first use, so it
inherits `app:app` ownership and just works. For a **bind mount**, prepare
the host directory for uid 999:

```sh
sudo install -d -o 999 -g 999 /srv/spherex-cache
docker run -d -e CACHE_DIR=/data/cache -v /srv/spherex-cache:/data/cache \
  -p 127.0.0.1:8080:8080 spherex-blink
```

## Behind a reverse proxy

The API rate limiter (`RATE_LIMIT_RPS`) keys on the client IP taken from
the `X-Forwarded-For` header when present, falling back to the TCP peer
address. That is correct behind Caddy, nginx or any proxy that sets the
header — per-visitor limits work as intended. It also means the limit is
**evadable if the container port is reachable directly**: a client that
can bypass the proxy simply spoofs `X-Forwarded-For`. Always publish the
port on localhost only (`-p 127.0.0.1:8080:8080`) or firewall it so every
request arrives through the proxy.

`ALLOWED_ORIGINS` is only needed when the frontend is hosted on a
**different origin** than the API (another domain, port or scheme). The
image serves the UI same-origin, so leave it empty — empty is the
strictest setting, no cross-origin requests at all.

## Quick start on a VPS

The build, `docker run` and smoke-verification commands below were executed
against the real image on 2026-11-15. Any Debian/Ubuntu VPS works.

```sh
# 1. Install Docker and Caddy.
curl -fsSL https://get.docker.com | sudo sh
sudo apt-get install -y caddy

# 2. Clone and build the image (~36 s clean build).
git clone <your-fork-url> spherex-blink && cd spherex-blink
docker build -t spherex-blink .

# 3. Run on localhost only, with a restart policy; Caddy exposes it.
docker run -d --name spherex-blink --restart unless-stopped \
  -p 127.0.0.1:8080:8080 spherex-blink
```

`/etc/caddy/Caddyfile` — replace the domain; Caddy obtains and renews the
TLS certificate automatically:

```caddy
blink.example.com {
	reverse_proxy 127.0.0.1:8080
}
```

```sh
sudo systemctl reload caddy
```

Firewall: only 22/80/443 need to be open; the app port is bound to
localhost so it is never exposed directly.

```sh
sudo ufw allow 22 && sudo ufw allow 80 && sudo ufw allow 443 && sudo ufw enable
```

Verify at any time: `scripts/docker-smoke.sh` runs the full check list
above against the local image. To check the live deployment, browse to
`https://blink.example.com/api/v1/health` — it must return
`{"status":"ok", ...}`.

## Hosts

### Render

1. New → Web Service → point at the repo. Render detects the `Dockerfile`.
2. Health check path: `/api/v1/health`.
3. Render injects `PORT` — the app already reads it; leave the default.
4. Optional: set env vars from the table above in the service settings.

### Fly.io

```sh
fly launch --no-deploy   # detects the Dockerfile, generates fly.toml
fly deploy
```

Minimal `fly.toml` essentials if writing it by hand:

```toml
app = "spherex-blink"

[build]                # uses the repo Dockerfile

[env]
  PORT = "8080"

[http_service]
  internal_port = 8080
  force_https = true
  auto_stop_machines = "stop"
  auto_start_machines = true
  min_machines_running = 1

  [http_service.concurrency]
    type = "requests"
    soft_limit = 200
    hard_limit = 250
```

### Azure Container Apps

```sh
az containerapp up \
  --name spherex-blink \
  --resource-group <rg> \
  --environment <aca-env> \
  --source .                 # builds the Dockerfile in the cloud
```

Or with a prebuilt image: `az containerapp create --image <registry>/spherex-blink:tag`.
Set ingress to `external`, `targetPort 8080`, and the health probe to
`GET /api/v1/health`.

### Railway

Railway auto-detects the Dockerfile on "Deploy from repo". For a plain VPS
with Docker, see "Quick start on a VPS" above.

## FortiGuard classification (required for judges)

Judges' corporate/school networks commonly run FortiGuard web filtering, and
freshly deployed hosting domains are frequently rated `Unrated`,
`Newly Registered Domain` or `Web Hosting` — categories many networks block.
The Space Apps submission guide therefore asks teams to classify the public
URL. Do this **as soon as the URL exists** (reviews take ~24 h):

1. Look up the deployed URL at https://www.fortiguard.com/webfilter
2. If it is unrated or in a blocked category, submit a reclassification at
   https://url.fortinet.net/rate/submit.php — suggest `Education` or
   `Information Technology`, note it is a NASA Space Apps hackathon project.
3. Re-check before submission and record the category in the project page.

## CI

`.github/workflows/ci.yml` runs on every push and PR:

- `backend`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (Swatinem/rust-cache).
- `supply-chain`: `cargo deny check licenses advisories bans` via
  `EmbarkStudios/cargo-deny-action@v2` against `backend/deny.toml`
  (license allow-list, RustSec advisories, bans). No secrets.
- `frontend`: `npm ci`, `npx tsc --noEmit`, `npx vitest run`, `npx eslint .`,
  `npm run build` (npm cache on `frontend/package-lock.json`).
- `docker`: `docker build` plus `scripts/docker-smoke.sh` — builds the
  image and runs the full container check list (API, SPA, gzip, headers,
  non-root, healthcheck, graceful stop). No push, no registry credentials.
