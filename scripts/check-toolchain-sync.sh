#!/usr/bin/env bash
# Fails when the pinned Rust or trunk version differs between the files that
# repeat it. rust-toolchain.toml is the source of truth for Rust; mise.toml is
# the source of truth for trunk. Needs only POSIX tools plus grep -E.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

first() { grep -Eom1 "$1" "$2" | head -n1; }

rust="$(first '^channel *= *"[^"]+"' rust-toolchain.toml | sed -E 's/.*"([^"]+)"/\1/')"
trunk="$(first '"github:trunk-rs/trunk" *= *"[^"]+"' mise.toml | sed -E 's/.*= *"([^"]+)"/\1/')"
[[ -n "$rust" && -n "$trunk" ]] || { echo "could not read pinned versions" >&2; exit 1; }

fail=0
check() { # label actual
  if [[ "$2" != "$1" ]]; then echo "mismatch: $3 has '$2', expected '$1'" >&2; fail=1; fi
}

check "$rust" "$(first '^rust-version *= *"[^"]+"' Cargo.toml | sed -E 's/.*"([^"]+)"/\1/')" "Cargo.toml rust-version"
check "$rust" "$(first '^FROM rust:[0-9.]+' Dockerfile | sed -E 's/FROM rust:([0-9.]+)/\1/')" "Dockerfile FROM rust:"
check "$trunk" "$(first '^ARG TRUNK_VERSION=[0-9.]+' Dockerfile | sed -E 's/.*=//')" "Dockerfile TRUNK_VERSION"

if (( fail )); then exit 1; fi
echo "versions in sync: rust $rust, trunk $trunk"
