#!/usr/bin/env bash
# Probe the five preset targets on a running SPHEREx Blink server and write
# a JSON summary per preset (counts, wavelengths, detectors, MJD span,
# truncated flag, response times, proper-motion check) into OUT_DIR.
#
# Two transports:
#   scripts/probe-presets.sh [--poll] BASE_URL OUT_DIR   # plain HTTP
#   scripts/probe-presets.sh [--poll] --docker NAME OUT_DIR
#                                                        # curl inside the
#                                                        # container (works
#                                                        # with --network none)
#
# --poll re-issues each request until the default selection is complete
# (raw: frames >= min(total_observations, 24); diff: frame count stable over
# 4 consecutive polls; spectrum: HTTP 200) or the deadline hits. Without
# --poll a single request per mode is issued — use it against a fully baked
# cache.
#
# Env:
#   BAKE_DEADLINE_EPOCH   unix time after which polling stops (default:
#                         now + PROBE_MAX_SECS)
#   PROBE_MAX_SECS        fallback budget when BAKE_DEADLINE_EPOCH is unset
#                         (default 900)
#   PROBE_LABEL           label stored in the JSON "served_from" field
#                         (default: http or baked-cache)
# Exits non-zero if any preset is missing data at the end.
set -euo pipefail

POLL=0
DOCKER_NAME=""
while [ $# -gt 0 ]; do
    case "$1" in
        --poll) POLL=1 ;;
        --docker) DOCKER_NAME="$2"; shift ;;
        *) break ;;
    esac
    shift
done

if [ -n "$DOCKER_NAME" ]; then
    BASE="http://127.0.0.1:8080"
    OUT="${1:?usage: probe-presets.sh [--poll] --docker NAME OUT_DIR}"
else
    BASE="${1:?usage: probe-presets.sh [--poll] BASE_URL OUT_DIR}"
    OUT="${2:?missing OUT_DIR}"
fi
DEADLINE="${BAKE_DEADLINE_EPOCH:-$(( $(date +%s) + ${PROBE_MAX_SECS:-900} ))}"
LABEL="${PROBE_LABEL:-$([ -n "$DOCKER_NAME" ] && echo baked-cache || echo http)}"
TMP="$(mktemp -d /tmp/spherex-probe.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT
command -v python3 >/dev/null || { echo "error: python3 required" >&2; exit 1; }
mkdir -p "$OUT"

# http_get PATH -> CODE RTIME; body lands in $TMP/body. Retries on 429.
CODE=000; RTIME=0
http_get() {
    local st i
    for i in 1 2 3 4 5; do
        if [ -n "$DOCKER_NAME" ]; then
            st="$(docker exec "$DOCKER_NAME" curl -sS --compressed --max-time 300 \
                -o /tmp/.probe-body -w '%{http_code} %{time_total}' "$BASE$1" \
                2>/dev/null || echo '000 0')"
            docker exec "$DOCKER_NAME" cat /tmp/.probe-body >"$TMP/body" 2>/dev/null \
                || : >"$TMP/body"
        else
            st="$(curl -sS --compressed --max-time 300 -o "$TMP/body" \
                -w '%{http_code} %{time_total}' "$BASE$1" 2>/dev/null || echo '000 0')"
        fi
        CODE="${st%% *}"; RTIME="${st##* }"
        [ "$CODE" != "429" ] && return
        sleep 1.3
    done
}

# frames_count FILE -> "n_frames total_observations"
frames_count() {
    python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    print(len(d.get("frames", [])), d.get("total_observations") or 0)
except Exception:
    print(0, 0)
PY
}

deadline_hit() { [ "$(date +%s)" -ge "$DEADLINE" ]; }

# request_frames QUERY MODE -> "nf tot first_s done_s cached_s code_last"
# Saves the last complete body to $TMP/frames-MODE.json
request_frames() {
    local q="$1" mode="$2" first="" prev=-1 stable=0 nf=0 tot=0 t0 exp cached=0
    t0=$(date +%s)
    while :; do
        http_get "/api/v1/frames?$q"
        [ -z "$first" ] && first="$RTIME"
        if [ "$CODE" = 200 ]; then
            read -r nf tot < <(frames_count "$TMP/body")
            exp=$(( tot < 24 ? tot : 24 ))
            if [ "$mode" = raw ]; then
                [ "$nf" -ge "$exp" ] && [ "$nf" -gt 0 ] && break
            fi
            if [ "$nf" = "$prev" ] && [ "$nf" -gt 0 ]; then
                stable=$((stable + 1))
            else
                stable=0
            fi
            prev=$nf
            [ "$stable" -ge 3 ] && break
        fi
        # A definitive 4xx will not improve by polling.
        { [ "$CODE" = 400 ] || [ "$CODE" = 404 ]; } && break
        [ "$POLL" = 1 ] || break
        deadline_hit && break
        sleep 15
    done
    cp "$TMP/body" "$TMP/frames-$mode.json" 2>/dev/null || true
    http_get "/api/v1/frames?$q"
    cached="$RTIME"
    printf '%s %s %s %s %s %s\n' "$nf" "$tot" "$first" \
        "$(( $(date +%s) - t0 ))" "$cached" "$CODE"
}

# summarize FILE -> JSON object on stdout (counts/wavelengths/detectors/mjd/truncated)
summarize_frames() {
    python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("{}"); sys.exit()
fs = d.get("frames", [])
wls = [f["wavelength_um"] for f in fs if isinstance(f.get("wavelength_um"), (int, float))]
mjds = [f["obs_mjd_start"] for f in fs] + [f["obs_mjd_end"] for f in fs]
out = {
    "frames": len(fs),
    "total_observations": d.get("total_observations"),
    "truncated": d.get("truncated"),
    "width": d.get("width"),
    "height": d.get("height"),
    "unit": d.get("unit"),
    "detectors": sorted({f["detector"] for f in fs if f.get("detector")}),
    "wavelength_um_min": min(wls) if wls else None,
    "wavelength_um_max": max(wls) if wls else None,
    "mjd_start_min": min(mjds) if mjds else None,
    "mjd_end_max": max(mjds) if mjds else None,
    "collections": sorted({c for f in fs for c in f.get("collection", "").split("+") if c}),
}
print(json.dumps(out))
PY
}

summarize_spectrum() {
    python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("{}"); sys.exit()
pts = d.get("points", [])
wls = [p["wavelength_um"] for p in pts if isinstance(p.get("wavelength_um"), (int, float))]
mjds = [p["obs_mjd"] for p in pts]
out = {
    "points": len(pts),
    "detectors": sorted({p["detector"] for p in pts if p.get("detector")}),
    "wavelength_um_min": min(wls) if wls else None,
    "wavelength_um_max": max(wls) if wls else None,
    "mjd_min": min(mjds) if mjds else None,
    "mjd_max": max(mjds) if mjds else None,
}
print(json.dumps(out))
PY
}

# Proper-motion check on a raw-mode /frames body: centroid of the brightest
# source in the inner field, earliest vs latest frame of the same detector
# at a matched wavelength; shift converted to arcsec via the frame WCS.
motion_check() {
    python3 - "$1" "$2" <<'PY'
import base64, json, math, struct, sys
EXPECTED = {
    "barnards-star": 10.3,
    "epsilon-indi-b": 4.7,
    "wise-0855": 8.2,
    "rw-aurigae": 0.02,
    "r-draconis": 0.02,
}
preset = sys.argv[2]
out = {"expected_motion_arcsec_per_yr": EXPECTED.get(preset)}
try:
    d = json.load(open(sys.argv[1]))
    fs = d.get("frames", [])
    w, h = d["width"], d["height"]
except Exception:
    out["motion"] = "not measured (no frames)"
    print(json.dumps(out)); sys.exit()

def pixels(f):
    return struct.unpack("<%df" % (w * h), base64.b64decode(f["data_b64"]))

def nan_frac(v):
    return sum(1 for x in v if x != x) / len(v)

# The preset target sits near the field centre and moves at most a few
# pixels over the survey; the brightest pixel inside this radius is the
# target (or noise — faint movers then honestly report a tiny shift).
CENTER_RADIUS = min(w, h) // 4

def centroid(v):
    cx, cy = w / 2.0, h / 2.0
    best = None
    for y in range(h):
        for x in range(w):
            if (x - cx) ** 2 + (y - cy) ** 2 > CENTER_RADIUS ** 2:
                continue
            val = v[y * w + x]
            if val == val and (best is None or val > best[0]):
                best = (val, x, y)
    if best is None:
        return None
    peak, px, py = best
    thr = peak * 0.3
    sx = sy = sw = 0.0
    for yy in range(max(0, py - 3), min(h, py + 4)):
        for xx in range(max(0, px - 3), min(w, px + 4)):
            val = v[yy * w + xx]
            if val == val and val > thr:
                sx += xx * val; sy += yy * val; sw += val
    return (sx / sw, sy / sw) if sw else None

def scale_arcsec(f):
    cd = f["wcs"]["cd"]
    det = cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]
    return math.sqrt(abs(det)) * 3600.0

decoded = {}
def get_pixels(f):
    key = id(f)
    if key not in decoded:
        decoded[key] = pixels(f)
    return decoded[key]

pairs = []
for det in {f["detector"] for f in fs}:
    # Skip heavily masked frames: with the central source masked out the
    # centroid locks onto a different object and invents a huge "motion".
    same = sorted((f for f in fs
                   if f["detector"] == det and nan_frac(get_pixels(f)) <= 0.3),
                  key=lambda f: f["obs_mjd_start"])
    if len(same) < 2:
        continue
    a = same[0]
    close = [b for b in same[1:]
             if abs(b["wavelength_um"] - a["wavelength_um"]) <= 0.05]
    if not close:
        continue  # a mismatched band centroid jumps between sources
    b = max(close, key=lambda f: f["obs_mjd_start"])
    dt = ((b["obs_mjd_start"] + b["obs_mjd_end"]) -
          (a["obs_mjd_start"] + a["obs_mjd_end"])) / 2.0
    if dt > 0:
        pairs.append((dt, det, a, b))
if not pairs:
    out["motion"] = "not measured (no same-detector epoch pair)"
    print(json.dumps(out)); sys.exit()
dt, det, a, b = max(pairs, key=lambda p: p[0])
ca, cb = centroid(get_pixels(a)), centroid(get_pixels(b))
if ca is None or cb is None:
    out["motion"] = "not measured (source not found)"
    print(json.dumps(out)); sys.exit()
s = (scale_arcsec(a) + scale_arcsec(b)) / 2.0
shift = math.hypot(cb[0] - ca[0], cb[1] - ca[1]) * s
dt_yr = dt / 365.25
out.update({
    "motion_pair_detector": det,
    "motion_pair_wavelength_um": [a["wavelength_um"], b["wavelength_um"]],
    "motion_pair_mjd": [a["obs_mjd_start"], b["obs_mjd_start"]],
    "motion_dt_yr": round(dt_yr, 3),
    "motion_shift_px": round(math.hypot(cb[0] - ca[0], cb[1] - ca[1]), 2),
    "measured_shift_arcsec": round(shift, 2),
    "measured_motion_arcsec_per_yr": round(shift / dt_yr, 2) if dt_yr else None,
})
print(json.dumps(out))
PY
}

# --- fetch the preset list -------------------------------------------------
http_get "/api/v1/targets"
[ "$CODE" = 200 ] || { echo "error: /api/v1/targets -> $CODE" >&2; exit 1; }
cp "$TMP/body" "$TMP/targets.json"
python3 - "$TMP/targets.json" > "$TMP/list.tsv" <<'PY'
import json, sys
for t in json.load(open(sys.argv[1])):
    print(f"{t['id']}\t{t['ra_deg']}\t{t['dec_deg']}\t{t['fov_deg']}")
PY

printf '%-16s %5s %5s %7s %7s %5s %8s %8s\n' \
    preset raw diff total sp_pts raw_s cached_s spec_s
rc=0
while IFS=$'\t' read -r id ra dec fov; do
    read -r nf tot first_s done_s cached_s last_code < \
        <(request_frames "ra=$ra&dec=$dec&fov=$fov&mode=raw" raw)
    raw_json="$(summarize_frames "$TMP/frames-raw.json")"

    read -r dnf dtot dfirst_s ddone_s dcached_s dlast_code < \
        <(request_frames "ra=$ra&dec=$dec&fov=$fov&mode=diff" diff)
    diff_json="$(summarize_frames "$TMP/frames-diff.json")"

    # spectrum at the preset position (default aperture 3)
    http_get "/api/v1/spectrum?ra=$ra&dec=$dec"
    spec_code="$CODE"; spec_s="$RTIME"
    while [ "$POLL" = 1 ] && [ "$spec_code" != 200 ] && [ "$spec_code" != 404 ] \
          && ! deadline_hit; do
        sleep 15
        http_get "/api/v1/spectrum?ra=$ra&dec=$dec"
        spec_code="$CODE"; spec_s="$RTIME"
    done
    cp "$TMP/body" "$TMP/spectrum.json" 2>/dev/null || true
    spec_json="$(summarize_spectrum "$TMP/spectrum.json")"

    motion_json="$(motion_check "$TMP/frames-raw.json" "$id")"

    python3 - "$OUT/$id.json" "$id" "$LABEL" "$raw_json" "$diff_json" \
        "$spec_json" "$motion_json" \
        "$first_s" "$done_s" "$cached_s" "$dfirst_s" "$ddone_s" \
        "$spec_s" "$spec_code" <<'PY'
import json, sys
(path, pid, label, raw, diff, spec, motion,
 raw_first, raw_done, raw_cached, diff_first, diff_done,
 spec_s, spec_code) = sys.argv[1:]
doc = {
    "preset": pid,
    "served_from": label,
    "raw": json.loads(raw),
    "diff": json.loads(diff),
    "spectrum": json.loads(spec),
    "spectrum_http": int(spec_code),
    "timings_s": {
        "raw_first_request": float(raw_first),
        "raw_complete_after": float(raw_done),
        "raw_cached_request": float(raw_cached),
        "diff_first_request": float(diff_first),
        "diff_complete_after": float(diff_done),
        "spectrum_request": float(spec_s),
    },
    "motion": json.loads(motion),
}
with open(path, "w") as fh:
    json.dump(doc, fh, indent=2)
    fh.write("\n")
PY

    printf '%-16s %5s %5s %7s %7s %5s %8s %8s\n' \
        "$id" "$nf" "$dnf" "$tot" \
        "$(python3 -c "import json;print(json.loads('''$spec_json''').get('points') or 0)")" \
        "$first_s" "$cached_s" "$spec_s"
    [ "$nf" -gt 0 ] || rc=1
done < "$TMP/list.tsv"

exit "$rc"
