#!/usr/bin/env bash
# Prints the workspace version (`version` in Cargo.toml's [workspace.package]) and fails unless it
# is semver (X.Y.Z, or X.Y.Z-pre for a pre-release). scripts/set-version.sh writes it.
# Usage: scripts/workspace-version.sh [FILE]   (default Cargo.toml; - reads stdin)
set -euo pipefail

src="${1:-Cargo.toml}"
file="${src}"
[[ "${file}" != - ]] || { file=/dev/stdin; src=stdin; }
version="$(sed -n '/^\[workspace\.package\]/,/^$/ s/^version = "\(.*\)"/\1/p' "${file}")"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] ||
  { echo "bad workspace version in ${src}: '${version}'" >&2; exit 1; }
printf '%s\n' "${version}"
