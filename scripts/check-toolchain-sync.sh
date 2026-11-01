#!/usr/bin/env bash
# Fails when the pinned Rust, CI mise, cosign or a task tool's version differs where it is repeated,
# when the Rust version is set anywhere else, when the docs repeat a pinned version, when a
# workflow builds an image without .github/actions/build-image, or when the advisory ignore lists
# disagree.
# rust-toolchain.toml is the source of truth for Rust (mise.lock records it for mise),
# .github/actions/setup-mise for the mise that the workflows, the Dockerfile and mise.toml's
# min_version use, and
# deny.toml for advisory ignores (mirrored in .cargo/audit.toml and osv-scanner.toml). Every
# other tool is pinned once, in mise.toml or mise.ci.toml. Needs only POSIX tools, grep -E and git.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# First match of an extended regex in a file; empty (not a failure) when absent.
first() { grep -Eom1 "$1" "$2" | head -n1 || true; }

rust="$(first '^channel *= *"[^"]+"' rust-toolchain.toml | sed -E 's/.*"([^"]+)"/\1/')"
mise="$(first '^ +version: *[0-9.]+$' .github/actions/setup-mise/action.yml | sed -E 's/.*: *//')"
[[ -n "$rust" && -n "$mise" ]] || { echo "could not read pinned versions" >&2; exit 1; }

fail=0
check() { # expected actual label
  if [[ "$2" != "$1" ]]; then echo "mismatch: $3 has '${2:-<nothing>}', expected '$1'" >&2; fail=1; fi
}

# mise.lock records the idiomatic rust version; `mise install --locked` fails if it is behind.
lock_rust="$(awk '/^\[\[tools\.rust\]\]/ {f=1; next} f && /^version = / {gsub(/[^0-9.]/, ""); print; exit}' mise.lock)"
check "$rust" "$lock_rust" "mise.lock [[tools.rust]]"
# No other copy of the Rust version, which a bump would then have to edit too. (The Dockerfile's
# rust image tag is not one: the build installs rust-toolchain.toml's toolchain.)
rust_copies="$(grep -lE '^rust-version' Cargo.toml crates/*/Cargo.toml fuzz/Cargo.toml 2>/dev/null || true)"
check "" "$(paste -sd' ' - <<<"$rust_copies")" "manifests with rust-version (rust-toolchain.toml sets the version)"
check "$mise" "$(first '^FROM ghcr.io/jdx/mise:[0-9.]+' Dockerfile | sed -E 's/.*://')" "Dockerfile FROM ghcr.io/jdx/mise:"
check "$mise" "$(first '^min_version = \{ hard = "[0-9.]+", soft = "[0-9.]+" \}' mise.toml | sed -E 's/.*"([0-9.]+)".*/\1/')" "mise.toml min_version"
# Workflows and other composite actions set up mise only through setup-mise, so its version is
# the only one to keep.
direct="$(grep -lE 'uses: *jdx/mise-action@' .github/workflows/*.y*ml .github/actions/*/action.y*ml 2>/dev/null |
  grep -vxF .github/actions/setup-mise/action.yml | paste -sd' ' - || true)"
check "" "$direct" "jdx/mise-action used directly (use \$/.github/actions/setup-mise)"
# Images are built through build-image, which holds the cache rules. The one other use is
# release.yml's push step, which publishes what its build-image step built and tested.
direct="$(grep -lE 'uses: *docker/build-push-action@' .github/workflows/*.y*ml .github/actions/*/action.y*ml 2>/dev/null |
  grep -vxF -e .github/actions/build-image/action.yml -e .github/workflows/release.yml | paste -sd' ' - || true)"
check "" "$direct" "docker/build-push-action used directly (use \$/.github/actions/build-image)"
release_builds="$(grep -cE 'uses: *docker/build-push-action@' .github/workflows/release.yml || true)"
if (( ${release_builds:-0} > 1 )); then
  echo "mismatch: release.yml uses docker/build-push-action directly $release_builds times (only its push step may; build through \$/.github/actions/build-image)" >&2
  fail=1
fi
# Task tools (`tools.NAME = "VERSION"`, or `tools = { NAME = "VERSION", ... }`, on a task in
# mise.toml): every task that uses a tool pins the same version, and so does a [tools] pin of it
# in mise.toml or mise.ci.toml (cosign, for the verify and up tasks, is CI's).
# "NAME VERSION" for each distinct task tool pin; a tool listed twice has two versions.
task_pins="$(
  { sed -nE 's/^tools\.([A-Za-z0-9_-]+) *= *"([^"]+)".*/\1 \2/p' mise.toml
    sed -nE 's/^tools *= *\{(.*)\}.*/\1/p' mise.toml | tr ',' '\n' |
      sed -nE 's/^ *([A-Za-z0-9_-]+) *= *"([^"]+)" *$/\1 \2/p'
  } | sort -u
)"
# A file's [tools] pin of a tool: `NAME = "VERSION"` or `NAME = { version = "VERSION", ... }`.
tools_pin() { # file name
  awk -v n="$2" '/^\[/ { t = ($0 == "[tools]") }
    t && $1 == n && $2 == "=" {
      s = $0; if (match(s, /version *= *"[^"]+"/)) s = substr(s, RSTART, RLENGTH)
      sub(/^[^"]*"/, "", s); sub(/".*/, "", s); print s; exit
    }' "$1"
}
synced=""
for name in $(cut -d' ' -f1 <<<"$task_pins" | sort -u); do
  pins="$(awk -v n="$name" '$1 == n { print $2 }' <<<"$task_pins" | paste -sd' ' -)"
  if [[ "$pins" == *' '* ]]; then
    echo "mismatch: mise.toml's tasks pin $name at different versions: $pins" >&2; fail=1; continue
  fi
  for f in mise.toml mise.ci.toml; do
    pinned="$(tools_pin "$f" "$name")"
    [[ -z "$pinned" ]] || check "$pinned" "$pins" "mise.toml tools.$name ($f [tools] has $pinned)"
  done
  synced+="${synced:+, }$name $pins"
done

# The docs never repeat a pinned version, which would go stale with the next bump: no
# "TOOL X.Y" for a tool pinned in [tools] of mise.toml or mise.ci.toml, a task tool, Rust, mise or
# Overpass, and no `rust:X` image tag. They say where the pin is instead. CHANGELOG.md records
# history and is exempt.
# An empty name would match any version number, so blank lines are dropped. A hyphen in a name
# also matches a space ("cargo nextest" for cargo-nextest), and the name may be in backticks.
names="$( { awk '/^\[/ { t = ($0 == "[tools]") } t && /^[A-Za-z0-9_-]+ *=/ { print $1 }' mise.toml mise.ci.toml
  cut -d' ' -f1 <<<"$task_pins"; printf '%s\n' rust mise overpass; } | sed '/^$/d' | sort -u |
  sed 's/-/[- ]/g' | paste -sd'|' -)"
# git grep exits 1 when nothing matches; anything above that is an error, not a pass. -o prints
# the offending text itself, which a long paragraph's line would otherwise bury.
rc=0
doc_pins="$(git grep --untracked -IinoE "(^|[^A-Za-z0-9_-])(${names})\`?( api)?[ @]v?[0-9]+\.[0-9]+|rust:[0-9][^ \`]*" -- '*.md' ':!CHANGELOG.md')" || rc=$?
(( rc <= 1 )) || { echo "could not search the docs for pinned versions (git grep exited $rc)" >&2; exit 1; }
if [[ -n "$doc_pins" ]]; then
  echo "pinned versions repeated in the docs (name the file that pins them instead):" >&2
  sed 's/^/  /' <<<"$doc_pins" >&2
  fail=1
fi

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
echo "versions in sync: rust $rust, mise $mise; task tools: ${synced:-none}; advisory ignores: ${deny_ids:-none}"
