#!/usr/bin/env bash
# Pre-warm the backend cache for the five preset targets and report
# cold vs warm latency per target (integration-live).
#
# Usage: scripts/warm.sh [BASE_URL] [MAX_TRIES]
#   BASE_URL   default http://localhost:8080
#   MAX_TRIES  follow-up requests per target, default 30 (20 s apart)
#
# Each /frames request returns what has decoded by the soft deadline and
# keeps downloading in the background, so the script re-requests until
# the default selection (24 frames, or fewer when fewer observations
# exist) is complete, then times one fully cached request. Requests are
# sequential: the backend itself bounds upstream concurrency. Run the
# server with CACHE_DIR set so the warm cache survives restarts.
set -euo pipefail
BASE="${1:-http://localhost:8080}"
TRIES="${2:-30}"
WANT=24
command -v python3 >/dev/null || { echo "python3 required" >&2; exit 1; }
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsS "$BASE/api/v1/targets" -o "$tmp/targets.json"
python3 - "$tmp/targets.json" > "$tmp/list.tsv" <<'PY'
import json, sys
for t in json.load(open(sys.argv[1])):
    print(f"{t['id']}\t{t['ra_deg']}\t{t['dec_deg']}\t{t['fov_deg']}")
PY

printf '%-16s %8s %10s %7s %7s %9s\n' target first_s complete_s frames total cached_s
while IFS=$'\t' read -r id ra dec fov; do
  q="ra=$ra&dec=$dec&fov=$fov&mode=raw"
  t0=$(date +%s.%N); first=""; frames=0; total="?"
  for _ in $(seq 1 "$TRIES"); do
    code=$(curl -s --compressed -o "$tmp/r.json" -w '%{http_code} %{time_total}' "$BASE/api/v1/frames?$q" || echo "000 0")
    set -- $code
    [ -z "$first" ] && first="$2"
    if [ "$1" = "200" ]; then
      read -r frames total < <(python3 -c "import json;d=json.load(open('$tmp/r.json'));print(len(d['frames']),d.get('total_observations'))")
      exp=$(( total < WANT ? total : WANT ))
      [ "$frames" -ge "$exp" ] && break
    fi
    sleep 20
  done
  done_s=$(python3 -c "print(round($(date +%s.%N)-$t0,1))")
  cached=$(curl -s --compressed -o /dev/null -w '%{time_total}' "$BASE/api/v1/frames?$q")
  printf '%-16s %8.1f %10s %7s %7s %9.3f\n' "$id" "$first" "$done_s" "$frames" "$total" "$cached"
done < "$tmp/list.tsv"
