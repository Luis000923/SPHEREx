# syntax=docker/dockerfile:1
# SPHEREx Blink: all-in-one image.
# Stage 0 picks up the optional baked preset cache, stage 1 builds the
# Vite/React frontend, stage 2 builds the Rust backend in release mode,
# stage 3 is a minimal runtime that serves both from one binary.

########## Stage 0: baked preset cache (optional) ##########
# scripts/bake-cache.sh fills cache-bake/ with the IRSA data of the five
# presets (SIA bodies + EPSF-stripped cutouts) so the demo does not depend
# on IRSA being up. The directory is not necessarily committed, so the copy
# goes through this stage: `docker build` still works without it and the
# image just starts with an empty /data/cache.
FROM debian:bookworm-slim AS baked-cache
WORKDIR /ctx
COPY . .
RUN mkdir -p /out && if [ -d cache-bake ]; then cp -a cache-bake/. /out/; fi

########## Stage 1: frontend build ##########
FROM node:22-bookworm-slim AS frontend-build
WORKDIR /build

# Install dependencies first so this layer is cached until the lockfile changes.
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund

# tsconfig.json covers src/, tests/ and vite.config.ts, so copy the whole tree.
COPY frontend/ ./
RUN npm run build

########## Stage 2: backend build ##########
# Full (non-slim) Rust image: ring/rustls build steps need a C toolchain.
FROM rust:1-bookworm AS backend-build
WORKDIR /build

# Compile dependencies against dummy sources so this layer is cached until
# Cargo.toml/Cargo.lock change; the real crate is rebuilt below.
COPY backend/Cargo.toml backend/Cargo.lock ./
RUN mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && printf '' > src/lib.rs \
    && cargo build --release \
    && rm -rf src

COPY backend/src ./src
# BuildKit COPY keeps context mtimes (older than the dummy build), so bump
# them to force cargo to rebuild the real crate instead of reusing the
# dummy-source artifacts.
RUN find src -type f -exec touch {} + \
    && cargo build --release --bin spherex-backend \
    && strip target/release/spherex-backend

########## Stage 3: runtime ##########
FROM debian:bookworm-slim AS runtime

# ca-certificates: outbound HTTPS to IRSA. curl: HEALTHCHECK only.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system app \
    && useradd --system --gid app --home-dir /app --no-create-home app

WORKDIR /app
COPY --from=backend-build /build/target/release/spherex-backend /app/spherex-backend
COPY --from=frontend-build /build/dist /app/static

# Persistent cache dir used when CACHE_DIR=/data/cache (see
# docker-compose.yml); created app-owned so a mounted volume inherits
# the right ownership. Harmless when CACHE_DIR is unset.
RUN mkdir -p /data/cache && chown app:app /data/cache

# Baked preset cache (stage 0): SIA bodies and EPSF-stripped cutouts for
# the five official targets. Empty when the build context had no
# cache-bake/ — the app then fills it on demand like before.
COPY --from=baked-cache --chown=app:app /out/ /data/cache/

# Entrypoint: refreshes the baked files' mtimes (the SIA disk cache counts
# its 24 h freshness from mtime) then execs the server as PID 1.
COPY --chmod=0755 scripts/docker-entrypoint.sh /app/docker-entrypoint.sh

# Defaults; see docs/DEPLOY.md for the full environment reference.
ENV PORT=8080 \
    STATIC_DIR=/app/static \
    RUST_LOG=info \
    MAX_CACHE_MB=200 \
    IRSA_MAX_CONCURRENCY=3 \
    CACHE_DIR=/data/cache

EXPOSE 8080
USER app

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT}/api/v1/health" || exit 1

CMD ["/app/docker-entrypoint.sh"]
