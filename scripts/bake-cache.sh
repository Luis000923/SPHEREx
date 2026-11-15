#!/usr/bin/env bash
# Bake the five preset targets into a persistent disk cache so the shipped
# image serves the demo without contacting IRSA (task b17).
#
# What it does:
#   1. builds the release backend (cargo build --release) when missing,
#   2. starts it with CACHE_DIR=<repo>/cache-bake and WARM_TARGETS=1 — the
#      startup warm-up fetches the raw-mode default selection (24 epochs)
#      of every preset through the disk cache,
#   3. waits for the "cache warm-up finished" log line, then drives
#      scripts/probe-presets.sh --poll, which requests /api/v1/frames in
#      modes raw and diff plus /api/v1/spectrum at each preset position,
#      polling until the default selections are complete — landing every
#      needed SIA body and cutout in the disk cache,
#   4. stops the server and prints the cache size and file count.
#
# Result in cache-bake/: search-*.xml (SIA/TAP result bodies, freshness
# window SEARCH_DISK_TTL = 24 h from file mtime — refreshed at container
# start by scripts/docker-entrypoint.sh), cutout-*.fits (EPSF-stripped
# cutouts, never expire) and *.meta coverage sidecars.
#
# Politeness: requests are sequential and the backend itself caps upstream
# concurrency at IRSA_MAX_CONCURRENCY (default 3) with retry/backoff.
#
# Usage: scripts/bake-cache.sh
# Env:
#   BAKE_CACHE_DIR   output directory, default ./cache-bake
#   BAKE_PORT        local port for the bake server, default 18180
#   BAKE_MAX_SECS    total budget for upstream waits, default 2400 (40 min).
#                    On expiry the script reports which presets are
#                    incomplete and exits 1 — results are never faked.
#   BAKE_FRESH=1     remove BAKE_CACHE_DIR first (clean rebuild)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/backend/target/release/spherex-backend"
CACHE_DIR="${BAKE_CACHE_DIR:-$ROOT/cache-bake}"
PORT="${BAKE_PORT:-18180}"
BASE="http://127.0.0.1:$PORT"
MAX_SECS="${BAKE_MAX_SECS:-2400}"
export BAKE_DEADLINE_EPOCH=$(( $(date +%s) + MAX_SECS ))

need() { command -v "$1" >/dev/null || { echo "error: $1 is required" >&2; exit 1; }; }
need cargo; need curl; need python3

if [ "${BAKE_FRESH:-0}" = "1" ]; then
    echo "==> BAKE_FRESH=1: removing $CACHE_DIR"
    rm -rf "$CACHE_DIR"
fi
mkdir -p "$CACHE_DIR"

if [ ! -x "$BIN" ]; then
    echo "==> Building release backend"
    (cd "$ROOT/backend" && cargo build --release)
fi

LOG="$(mktemp /tmp/spherex-bake.XXXXXX.log)"
STATIC_DIR_TMP="$(mktemp -d /tmp/spherex-bake-static.XXXXXX)"
RESULTS_DIR="$(mktemp -d /tmp/spherex-bake-results.XXXXXX)"
SRV_PID=""

cleanup() {
    [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true
    rm -rf "$STATIC_DIR_TMP"
    echo "==> Server log kept at $LOG"
    echo "==> Per-preset results kept at $RESULTS_DIR"
}
trap cleanup EXIT

deadline_hit() { [ "$(date +%s)" -ge "$BAKE_DEADLINE_EPOCH" ]; }

echo "==> Starting release backend on $BASE (CACHE_DIR=$CACHE_DIR, WARM_TARGETS=1)"
PORT="$PORT" \
CACHE_DIR="$CACHE_DIR" \
WARM_TARGETS=1 \
STATIC_DIR="$STATIC_DIR_TMP" \
RUST_LOG="${RUST_LOG:-info}" \
RATE_LIMIT_RPS=10 \
REQUEST_DEADLINE_SECS=180 \
REQUEST_SOFT_DEADLINE_SECS=10 \
IRSA_FAILURE_TTL_SECS=60 \
"$BIN" >"$LOG" 2>&1 &
SRV_PID=$!

echo "==> Waiting for /api/v1/health"
for _ in $(seq 1 60); do
    curl -fsS --max-time 5 "$BASE/api/v1/health" >/dev/null 2>&1 && break
    kill -0 "$SRV_PID" 2>/dev/null || { echo "error: backend died; log:" >&2; cat "$LOG" >&2; exit 1; }
    sleep 1
done
curl -fsS --max-time 5 "$BASE/api/v1/health" >/dev/null \
    || { echo "error: backend never became healthy; log:" >&2; cat "$LOG" >&2; exit 1; }
echo "==> Backend is up"

echo "==> Waiting for the startup warm-up ('cache warm-up finished' log line)"
until grep -q "cache warm-up finished" "$LOG"; do
    kill -0 "$SRV_PID" 2>/dev/null || { echo "error: backend died; log:" >&2; cat "$LOG" >&2; exit 1; }
    if deadline_hit; then
        echo "error: bake deadline (${MAX_SECS}s) hit while warming; log tail:" >&2
        tail -20 "$LOG" >&2
        exit 1
    fi
    sleep 5
done
grep "cache warm-up finished" "$LOG" || true

echo "==> Probing all five presets (frames raw+diff, spectrum) until complete"
PROBE_RC=0
"$ROOT/scripts/probe-presets.sh" --poll "$BASE" "$RESULTS_DIR" || PROBE_RC=$?

echo "==> Stopping the bake server"
kill -TERM "$SRV_PID" 2>/dev/null || true
wait "$SRV_PID" 2>/dev/null || true
SRV_PID=""

echo "==> Baked cache: $CACHE_DIR"
du -sh "$CACHE_DIR"
printf 'files: %s\n' "$(find "$CACHE_DIR" -type f | wc -l)"
for ext in xml fits meta; do
    printf '  %-6s %s files, %s\n' "$ext" \
        "$(find "$CACHE_DIR" -name "*.$ext" | wc -l)" \
        "$(du -ch "$CACHE_DIR"/*."$ext" 2>/dev/null | tail -1 | cut -f1 || echo 0)"
done

[ "$PROBE_RC" -eq 0 ] || {
    echo "error: bake incomplete — see per-preset results in $RESULTS_DIR" >&2
    exit 1
}
echo "==> Bake complete"
