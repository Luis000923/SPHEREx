#!/bin/sh
# Container entrypoint for the all-in-one image.
#
# The disk cache treats SIA/TAP result bodies (search-*.xml) as fresh only
# for SEARCH_DISK_TTL_SECS (default 86400 s) counted from the file mtime
# (backend/src/irsa/client.rs `disk_cached_get`/`disk_observations`); once
# stale they are refetched upstream. Since b20 a transient refetch
# failure serves the stale body, so the baked presets already keep
# answering when IRSA is unreachable — e.g. `docker run --network none` —
# and this mtime refresh is belt-and-braces rather than required. It is
# still kept: fresh mtimes skip the pointless upstream refetch attempt on
# every request and let new ingests land the moment the network is back.
# Cutout files (cutout-*.fits) have no expiry; touching them is harmless.
# A mounted volume gets the same treatment.
set -eu

refresh() {
    dir="${CACHE_DIR:-/data/cache}"
    [ -d "$dir" ] && find "$dir" -type f -exec touch {} + 2>/dev/null || true
}

refresh
# Hourly refresh covers containers kept running longer than the TTL.
( while :; do sleep 3600; refresh; done ) &

exec /app/spherex-backend "$@"
