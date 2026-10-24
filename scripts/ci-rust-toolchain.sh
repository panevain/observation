#!/usr/bin/env bash
# Installs the toolchain that rust-toolchain.toml in the current directory pins (plus any extra
# components given as arguments), then uninstalls every other toolchain. Swatinem/rust-cache
# hashes the `rustc -vV` of each installed toolchain into its cache key, and the runner image's
# preinstalled stable changes with each image update, so leaving it there gives jobs that run
# on different image versions different keys and makes them miss develop's caches.
# Usage: scripts/ci-rust-toolchain.sh [COMPONENT...]
set -euo pipefail

rustup toolchain install --profile minimal --no-self-update
for c in "$@"; do rustup component add "${c}"; done
pinned="$(rustup show active-toolchain | cut -d' ' -f1)"
rustup toolchain list --quiet | { grep -vxF "${pinned}" || true; } | while read -r t; do
  rustup toolchain uninstall "${t}"
done
rustup toolchain list
