#!/usr/bin/env bash
# Records the Terrarium elevation tiles the demo street's terrain is sampled
# from. Run from this directory; replaces demo-dem-*.png. The tile list must
# match `dem_tiles_for` on demo-bbox.txt (the `demo_dem_tiles_match_the_bbox`
# test in src/demo.rs checks it).
set -euo pipefail
default='https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png'
template="${OBS_DEM_URL:-${default}}"
for tile in 15/5260/11429 15/5260/11430; do
  IFS=/ read -r z x y <<< "${tile}"
  url="${template//\{z\}/${z}}"
  url="${url//\{x\}/${x}}"
  url="${url//\{y\}/${y}}"
  out="demo-dem-${z}-${x}-${y}.png"
  curl -fsS -m 60 -A "observation-fixture/0.1 (+https://github.com/panevain/observation)" \
    -o "${out}.tmp" "${url}"
  mv "${out}.tmp" "${out}"
done
date -u +%F
