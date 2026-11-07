#!/usr/bin/env bash
# Rebuilds the expected GNSS output by running the original gnss-ppsd awk scripts
# (reference.awk, reference-clients.awk) on the inputs in this directory.
# Needs an awk with strftime (gawk, busybox) or the stand-in below for BSD awk.
set -euo pipefail
cd "$(dirname "$0")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp tracking.txt "$work/.tracking"
cp sources.txt "$work/.sources"
cp sourcestats.txt "$work/.sourcestats"
cp serverstats.txt "$work/.serverstats"
cp steps.txt "$work/.steps"
cat gga.txt gsa.txt gsv.txt > "$work/.nmea"
script=reference.awk
if ! awk 'BEGIN { strftime("%Y", 0, 1) }' >/dev/null 2>&1; then
    # BSD awk has no strftime; only "%Y-%m-%d %H:%M" in UTC is used.
    script="$work/reference.awk"
    {
        printf '%s\n' 'function strftime(f, t, u, c, s) { c = "date -u -r " t " \"+" f "\""; c | getline s; close(c); return s }'
        cat reference.awk
    } > "$script"
fi
awk -f "$script" -v dir="$work" -v now=1793810968 -v append=1 -v at_ok=1 \
    -v chrony_up=1860 -v started=2026-11-04T16:48:28Z -v conf=chrony.conf \
    -v fw=RM520NGLAAR03A04M4G_A0.303.A0.303 > expected-live.json
for f in track stats sat snr dop pps snrhist; do
    printf '#%s\n' "$f"
    cat "$work/$f.log" 2>/dev/null || true
done > expected-history.txt
awk -f reference-clients.awk clients.txt > expected-clients.json
echo "expected-live.json, expected-history.txt, expected-clients.json"
