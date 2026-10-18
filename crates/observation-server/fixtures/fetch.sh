#!/usr/bin/env bash
# Records ONE real Overpass response for the bbox in demo-bbox.txt.
# Run from this directory; replaces demo-overpass.json. Be polite: one request,
# and wait >= 30 s before retrying a failure.
set -euo pipefail
bbox="$(tr -d '[:space:]' < demo-bbox.txt)"
query="[out:json][timeout:25];
(
  way[\"building\"](${bbox});
  relation[\"building\"][\"type\"=\"multipolygon\"](${bbox});
  way[\"highway\"](${bbox});
);
out geom qt;"
curl -fsS -m 60 -A "observation-fixture/0.1 (+https://github.com/panevain/observation)" \
  --data-urlencode "data=${query}" \
  -o demo-overpass.json.tmp "${OBS_OVERPASS_URL:-https://overpass-api.de/api/interpreter}"
mv demo-overpass.json.tmp demo-overpass.json
date -u +%F
