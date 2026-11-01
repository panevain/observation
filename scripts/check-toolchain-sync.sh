#!/usr/bin/env bash
# Fails when the pinned CI mise or cosign version differs between the files that repeat it, when
# the Rust version is set anywhere but rust-toolchain.toml (which Dependabot bumps alone), or when
# the advisory ignore lists disagree. .github/actions/setup-mise is the source of truth for the
# mise that the workflows, the Dockerfile and mise.toml's min_version use, and
# deny.toml for advisory ignores (mirrored in .cargo/audit.toml and osv-scanner.toml). Every
# other tool is pinned once, in mise.toml or mise.ci.toml. Needs only POSIX tools plus grep -E.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# First match of an extended regex in a file; empty (not a failure) when absent.
first() { grep -Eom1 "$1" "$2" | head -n1 || true; }

mise="$(first '^ +version: *[0-9.]+$' .github/actions/setup-mise/action.yml | sed -E 's/.*: *//')"
[[ -n "$mise" ]] || { echo "could not read the CI mise version" >&2; exit 1; }

fail=0
check() { # expected actual label
  if [[ "$2" != "$1" ]]; then echo "mismatch: $3 has '${2:-<nothing>}', expected '$1'" >&2; fail=1; fi
}

# Rust only in rust-toolchain.toml: a second copy would go stale on every Dependabot bump. (The
# Dockerfile's rust image tag is not one: the build installs rust-toolchain.toml's toolchain.)
rust_copies="$(grep -lE '^rust-version *=|^rust-version\.workspace' Cargo.toml crates/*/Cargo.toml fuzz/Cargo.toml 2>/dev/null || true;
  grep -lE '^\[\[tools\.rust\]\]' mise.lock mise.ci.lock || true;
  grep -lE '^idiomatic_version_file_enable_tools.*"rust"' mise*.toml || true)"
check "" "$(paste -sd' ' - <<<"$rust_copies")" "files repeating the Rust version (only rust-toolchain.toml sets it)"
check "$mise" "$(first '^FROM ghcr.io/jdx/mise:[0-9.]+' Dockerfile | sed -E 's/.*://')" "Dockerfile FROM ghcr.io/jdx/mise:"
check "$mise" "$(first '^min_version = \{ hard = "[0-9.]+", soft = "[0-9.]+" \}' mise.toml | sed -E 's/.*"([0-9.]+)".*/\1/')" "mise.toml min_version"
# Workflows and other composite actions set up mise only through setup-mise, so its version is
# the only one to keep.
direct="$(grep -lE 'uses: *jdx/mise-action@' .github/workflows/*.y*ml .github/actions/*/action.y*ml 2>/dev/null |
  grep -vxF .github/actions/setup-mise/action.yml | paste -sd' ' - || true)"
check "" "$direct" "jdx/mise-action used directly (use \$/.github/actions/setup-mise)"
# cosign is a CI tool (mise.ci.toml) and a task tool of mise.toml's verify and up tasks.
cosign="$(first '^cosign *= *"[^"]+"' mise.ci.toml | sed -E 's/.*"([^"]+)"/\1/')"
while read -r task_cosign; do
  check "$cosign" "$task_cosign" "mise.toml tools.cosign (mise.ci.toml has cosign $cosign)"
done < <(grep -E '^tools\.cosign *= *"' mise.toml | sed -E 's/.*"([^"]+)"/\1/')

# Advisory ignores: the same IDs in all three files (sorted, one per line).
# grep finds nothing when a list is empty, which is not a failure.
ids() { { grep -Eo 'RUSTSEC-[0-9]{4}-[0-9]{4}|GHSA(-[0-9a-z]{4}){3}' || true; } | sort -u | paste -sd' ' -; }
# What is inside a file's `ignore = [ ... ]`, written on one line or over several (closed by a
# line starting with `]`, so a `]` inside a reason string does not end the list).
ignores() {
  awk '/^ignore *= *\[/ { sub(/^ignore *= *\[/, ""); if (sub(/\][^]]*$/, "")) { print; exit } f=1; print; next }
       f && /^ *\]/ { exit } f' "$1"
}
deny_ids="$(ignores deny.toml | sed '/^ *#/d' | ids)"
check "$deny_ids" "$(ignores .cargo/audit.toml | sed 's/#.*//' | ids)" ".cargo/audit.toml ignore"
check "$deny_ids" "$(sed -n '/^id *=/p' osv-scanner.toml | ids)" "osv-scanner.toml IgnoredVulns"

if (( fail )); then exit 1; fi
echo "versions in sync: mise $mise, cosign $cosign (Rust only in rust-toolchain.toml); advisory ignores: ${deny_ids:-none}"
