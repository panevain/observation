#!/usr/bin/env bash
# Fails when the pinned Rust or CI mise version differs between the files that repeat it,
# or when the advisory ignore lists disagree. rust-toolchain.toml is the source of truth for
# Rust, MISE_VERSION in ci.yml for the mise that the workflows and the Dockerfile use, and
# deny.toml for advisory ignores (mirrored in .cargo/audit.toml and osv-scanner.toml). Every
# other tool is pinned once, in mise.toml or mise.ci.toml. Needs only POSIX tools plus grep -E.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# First match of an extended regex in a file; empty (not a failure) when absent.
first() { grep -Eom1 "$1" "$2" | head -n1 || true; }

rust="$(first '^channel *= *"[^"]+"' rust-toolchain.toml | sed -E 's/.*"([^"]+)"/\1/')"
mise="$(first '^  MISE_VERSION: *[0-9.]+' .github/workflows/ci.yml | sed -E 's/.*: *//')"
[[ -n "$rust" && -n "$mise" ]] || { echo "could not read pinned versions" >&2; exit 1; }

fail=0
check() { # expected actual label
  if [[ "$2" != "$1" ]]; then echo "mismatch: $3 has '${2:-<nothing>}', expected '$1'" >&2; fail=1; fi
}

check "$rust" "$(first '^rust-version *= *"[^"]+"' Cargo.toml | sed -E 's/.*"([^"]+)"/\1/')" "Cargo.toml rust-version"
check "$rust" "$(first '^FROM rust:[0-9.]+' Dockerfile | sed -E 's/FROM rust:([0-9.]+)/\1/')" "Dockerfile FROM rust:"
# mise.lock records the idiomatic rust version; `mise install --locked` fails if it is behind.
lock_rust="$(awk '/^\[\[tools\.rust\]\]/ {f=1; next} f && /^version = / {gsub(/[^0-9.]/, ""); print; exit}' mise.lock)"
check "$rust" "$lock_rust" "mise.lock [[tools.rust]]"
check "$mise" "$(first '^FROM ghcr.io/jdx/mise:[0-9.]+' Dockerfile | sed -E 's/.*://')" "Dockerfile FROM ghcr.io/jdx/mise:"
# Every mise-action step must pin `version:`, either to MISE_VERSION (in ci.yml) or to the
# same literal. A step without one would install whatever mise is latest.
for wf in .github/workflows/*.yml .github/workflows/*.yaml; do
  [[ -f "$wf" ]] || continue
  while read -r v; do
    [[ "$wf" == .github/workflows/ci.yml && "$v" == '${{ env.MISE_VERSION }}' ]] && continue
    check "$mise" "$v" "$wf mise-action version"
  done < <(awk '
    function done_step() { if (inside) print (v == "" ? "<none>" : v); inside = 0 }
    /uses: *jdx\/mise-action@/ { done_step(); inside = 1; v = ""; next }
    inside && /^ *- / { done_step() }
    inside && /^ *version:/ {
      v = $0; sub(/^ *version: */, "", v); sub(/ +#.*$/, "", v); gsub(/^["\x27]|["\x27]$/, "", v)
    }
    END { done_step() }
  ' "$wf")
done

# Advisory ignores: the same IDs in all three files (sorted, one per line).
ids() { grep -Eo 'RUSTSEC-[0-9]{4}-[0-9]{4}|GHSA(-[0-9a-z]{4}){3}' | sort -u | paste -sd' ' -; }
deny_ids="$(awk '/^ignore = \[/ {f=1; next} f && /^\]/ {exit} f && !/^ *#/' deny.toml | ids)"
check "$deny_ids" "$(awk '/^ignore = \[/ {f=1; next} f && /^\]/ {exit} f' .cargo/audit.toml | sed 's/#.*//' | ids)" ".cargo/audit.toml ignore"
check "$deny_ids" "$(grep -E '^id *=' osv-scanner.toml | ids)" "osv-scanner.toml IgnoredVulns"

if (( fail )); then exit 1; fi
echo "versions in sync: rust $rust, mise $mise; advisory ignores: ${deny_ids:-none}"
