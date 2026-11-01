#!/usr/bin/env bash
# Sets the workspace version (Cargo.toml [workspace.package]) and refreshes both lockfiles'
# entries for the workspace crates, in the checkout in the current directory (hotfix.yml runs
# it on a checkout of main). Used by release-prep.yml and hotfix.yml.
# Usage: scripts/set-version.sh VERSION   (semver without the leading v, e.g. 0.2.0)
set -euo pipefail

version="${1:?usage: $0 VERSION}"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "not a semver version: ${version}" >&2; exit 1; }
sed -i -E '/^\[workspace\.package\]/,/^$/ s/^version = "[^"]+"/version = "'"${version}"'"/' Cargo.toml
[ "$("$(dirname "${BASH_SOURCE[0]}")/workspace-version.sh")" = "${version}" ] ||
  { echo "could not set the version in Cargo.toml [workspace.package]" >&2; exit 1; }
cargo update -w
cargo update -w --manifest-path fuzz/Cargo.toml
