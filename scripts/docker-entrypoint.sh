#!/bin/sh
# Container entrypoint for the all-in-one image.
#
# The disk cache treats SIA/TAP result bodies (search-*.xml) as fresh only
# for SEARCH_DISK_TTL = 24 h counted from the file mtime
# (backend/src/irsa/client.rs `disk_cached_get`/`disk_observations`); once
# stale they are refetched upstream and, today, there is no
# serve-stale-on-error fallback. The image ships preset data baked by
# scripts/bake-cache.sh: refreshing mtimes at start (and hourly) keeps the
# baked search bodies "fresh" for the whole container lifetime, so the
# presets keep answering when IRSA is unreachable — e.g. `docker run
# --network none`. Cutout files (cutout-*.fits) have no expiry; touching
# them is harmless. A mounted volume gets the same treatment.
set -eu

refresh() {
    dir="${CACHE_DIR:-/data/cache}"
    [ -d "$dir" ] && find "$dir" -type f -exec touch {} + 2>/dev/null || true
}

refresh
# Hourly refresh covers containers kept running longer than the TTL.
( while :; do sleep 3600; refresh; done ) &

exec /app/spherex-backend "$@"
