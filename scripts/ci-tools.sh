#!/usr/bin/env bash
# Pinned CI tools, installed from their release archives and checked against a pinned sha256.
# This table is the only place their versions and checksums live: the workflows install
# through this script, and scripts/bump-pins.sh (the Pins workflow) rewrites the table.
# The checksum is GitHub's sha256 digest of the linux x86_64 release asset, which matches
# the checksum files the projects publish.
#
# Usage:
#   scripts/ci-tools.sh install TOOL...   install into CI_TOOLS_BIN (default ~/.local/bin),
#                                         which is added to GITHUB_PATH when that is set
#   scripts/ci-tools.sh version TOOL      print the pinned version
set -euo pipefail

# name          version   sha256
TOOLS="
trivy           0.76.1    c7cbe3137fb8a48a17574bede3dd6beb16742d1e9772929940fc90082c39d9c0
cosign          3.1.4     424e2c97d0b45fd7a6b608a87f090f35633f4c23ece5f092d48bba11aaf4bbfa
cargo-audit     0.22.2    7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428
cargo-nextest   0.9.149   32d1d8c0567b7a80bee2bad8bb09125c846da96bc009b2de11ec08557290c770
cargo-llvm-cov  0.9.1     b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a
cargo-fuzz      0.13.2    -
"

# GitHub repository, release tag and asset name for a tool and version; cargo-fuzz has no
# prebuilt release and is built with `cargo install --locked` instead.
source_of() { # name version -> "repo tag asset"
  local v="$2"
  case "$1" in
    trivy) echo "aquasecurity/trivy v${v} trivy_${v}_Linux-64bit.tar.gz" ;;
    cosign) echo "sigstore/cosign v${v} cosign-linux-amd64" ;;
    cargo-audit) echo "rustsec/rustsec cargo-audit/v${v} cargo-audit-x86_64-unknown-linux-musl-v${v}.tgz" ;;
    cargo-nextest) echo "nextest-rs/nextest cargo-nextest-${v} cargo-nextest-${v}-x86_64-unknown-linux-gnu.tar.gz" ;;
    cargo-llvm-cov) echo "taiki-e/cargo-llvm-cov v${v} cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz" ;;
    *) return 1 ;;
  esac
}

pin() { # name -> "version sha256"
  local line
  line="$(awk -v n="$1" '$1 == n { print $2, $3 }' <<<"${TOOLS}")"
  [[ -n "${line}" ]] || { echo "unknown tool: $1" >&2; exit 2; }
  echo "${line}"
}

install_one() { # name dest
  local name="$1" dest="$2" version sha repo tag asset tmp
  read -r version sha < <(pin "${name}")
  if [[ "${name}" == cargo-fuzz ]]; then
    if cargo fuzz --version 2>/dev/null | grep -qF "${version}"; then return 0; fi
    cargo install --locked --version "${version}" cargo-fuzz
    return 0
  fi
  read -r repo tag asset < <(source_of "${name}" "${version}")
  tmp="$(mktemp -d)"
  curl --proto '=https' --tlsv1.2 -fsSL -o "${tmp}/${asset}" \
    "https://github.com/${repo}/releases/download/${tag}/${asset}"
  echo "${sha}  ${tmp}/${asset}" | sha256sum -c --quiet -
  case "${asset}" in
    *.tar.gz | *.tgz)
      # Every archive holds a single binary named after the tool (cargo-audit's is in a
      # versioned directory).
      tar -xzf "${tmp}/${asset}" -C "${tmp}" --wildcards --no-anchored "${name}"
      install -m 0755 "$(find "${tmp}" -type f -name "${name}" | head -n1)" "${dest}/${name}"
      ;;
    *) install -m 0755 "${tmp}/${asset}" "${dest}/${name}" ;;
  esac
  rm -rf "${tmp}"
  echo "installed ${name} ${version}"
}

cmd="${1:?usage: $0 install TOOL... | version TOOL}"
shift
case "${cmd}" in
  install)
    dest="${CI_TOOLS_BIN:-${HOME}/.local/bin}"
    mkdir -p "${dest}"
    if [[ -n "${GITHUB_PATH:-}" ]]; then echo "${dest}" >>"${GITHUB_PATH}"; fi
    for t in "$@"; do install_one "${t}" "${dest}"; done
    ;;
  version) read -r v _ < <(pin "${1:?tool}") && echo "${v}" ;;
  source) source_of "${1:?tool}" "${2:?version}" ;;
  *) echo "unknown command ${cmd}" >&2; exit 2 ;;
esac
