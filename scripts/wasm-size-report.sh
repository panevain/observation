#!/usr/bin/env bash
# Regenerate docs/images/wasm-bundle-treemap.svg: the shipped WebAssembly
# bundle tiled by size per dependency crate.
#
# Usage: scripts/wasm-size-report.sh [--no-build]
#
# Builds observation-client with the same `wasm-release` profile the release
# build uses (see web/index.html), changing only what is needed to keep symbol
# names: strip = false and no debug info. Sizes are unaffected because code
# generation is identical. The module is then post-processed like trunk does
# (wasm-bindgen CLI matching Cargo.lock, then wasm-opt -Os with the feature
# flags web/index.html gives trunk, plus --debuginfo to keep the name section)
# and attributed to crates by scripts/wasm-size-treemap.py.
#
# A fat-LTO build takes 15-20 minutes on four cores. --no-build reuses the
# existing named module in the target directory.
#
# Environment:
#   CARGO_TARGET_DIR  cargo target directory (default: ./target)
#   WASM_BINDGEN      path to a wasm-bindgen CLI (default: PATH, then trunk's cache)
#   WASM_OPT          path to wasm-opt (default: PATH, then trunk's cache)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

build=1
[[ "${1:-}" == "--no-build" ]] && build=0

target_dir="${CARGO_TARGET_DIR:-$root/target}"
profile=wasm-release
named="$target_dir/wasm32-unknown-unknown/$profile/observation-client.wasm"
out_svg="docs/images/wasm-bundle-treemap.svg"

if (( build )); then
  CARGO_PROFILE_WASM_RELEASE_STRIP=false CARGO_PROFILE_WASM_RELEASE_DEBUG=false \
    cargo build --locked -p observation-client --bin observation-client --target wasm32-unknown-unknown --profile "$profile"
fi
[[ -f "$named" ]] || { echo "missing $named (run without --no-build)" >&2; exit 1; }

# Locate tools the way trunk does: PATH first, then trunk's download cache.
cache_dirs=("${XDG_CACHE_HOME:-$HOME/.cache}/trunk" "$HOME/Library/Caches/dev.trunkrs.trunk")
find_tool() { # name, glob-suffix
  local name="$1" glob="$2" d f
  if command -v "$name" >/dev/null 2>&1; then command -v "$name"; return 0; fi
  for d in "${cache_dirs[@]}"; do
    for f in $d/$glob; do [[ -x "$f" ]] && { echo "$f"; return 0; }; done
  done
  return 1
}

bindgen_ver="$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/[^0-9.]/, "", $0); print; exit}' Cargo.lock)"
bindgen="${WASM_BINDGEN:-$(find_tool wasm-bindgen "wasm-bindgen-$bindgen_ver/wasm-bindgen" || true)}"
wasm_opt="${WASM_OPT:-$(find_tool wasm-opt "wasm-opt-version_*/bin/wasm-opt" || true)}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
module="$named"
stage="before wasm-bindgen and wasm-opt"

if [[ -n "$bindgen" ]] && [[ "$("$bindgen" --version)" == *"$bindgen_ver" ]]; then
  "$bindgen" --target web --no-typescript --out-dir "$work" --out-name observation-client "$named"
  module="$work/observation-client_bg.wasm"
  stage="after wasm-bindgen"
else
  echo "warning: wasm-bindgen $bindgen_ver not found; measuring the raw cargo output" >&2
fi

# The feature flags trunk passes to wasm-opt: data-wasm-opt-params on the client's link.
read -ra opt_params <<<"$(sed -nE '/data-bin="observation-client"/ s/.*data-wasm-opt-params="([^"]*)".*/\1/p' web/index.html)"
(( ${#opt_params[@]} )) || { echo "no data-wasm-opt-params for observation-client in web/index.html" >&2; exit 1; }

if [[ -n "$wasm_opt" && "$stage" == "after wasm-bindgen" ]]; then
  "$wasm_opt" -Os --debuginfo "${opt_params[@]}" "$module" -o "$work/opt.wasm"
  module="$work/opt.wasm"
  stage="after wasm-bindgen and wasm-opt -Os"
else
  echo "warning: wasm-opt not found; sizes are before optimisation" >&2
fi

mkdir -p "$(dirname "$out_svg")"
python3 -I scripts/wasm-size-treemap.py \
  --wasm "$module" --out "$out_svg" --stage "$stage" \
  --commit "$(git rev-parse --short=9 HEAD)" \
  --date "$(git log -1 --format=%cs HEAD)" \
  --table 15

echo
echo "wrote $out_svg"
for f in dist/*_bg.wasm; do
  [[ -f "$f" ]] && echo "for comparison, $f: $(wc -c <"$f") B raw, $(gzip -9 -c "$f" | wc -c) B gzip"
done
exit 0
