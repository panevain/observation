#!/usr/bin/env bash
# Design-token guard: the Bevy/egui client must take every colour from
# crates/observation-client/src/theme.rs (which takes the UI colour roles
# from crates/observation-tokens). Any other .rs file under
# crates/observation-client/src/ that hard-codes a colour literal fails the
# check. Prints file:line:match for each offence and exits 1.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src_dir="${root}/crates/observation-client/src"
theme_file="${src_dir}/theme.rs"

# One ERE alternation; grep -E is POSIX (no PCRE needed).
patterns=(
  'Color32::from_rgb'
  'Color32::from_rgba'
  'Color32::from_gray'
  'Color::srgb\('
  'Color::srgba\('
  'Color::rgb\('
  'Srgba::new\('
  'Srgba::rgb\('
  'Color32::[A-Z][A-Z0-9_]*'
  '"[^"]*#[0-9a-fA-F]{6}'
)
regex="$(IFS='|'; echo "${patterns[*]}")"

status=0
while IFS= read -r -d '' file; do
  [ "${file}" = "${theme_file}" ] && continue
  if hits="$(grep -nE -- "${regex}" "${file}")"; then
    while IFS= read -r line; do
      echo "${file#"${root}"/}:${line}"
    done <<<"${hits}"
    status=1
  fi
done < <(find "${src_dir}" -type f -name '*.rs' -print0 | sort -z)

if [ "${status}" -ne 0 ]; then
  echo >&2
  echo "check-design-tokens: hard-coded colour literals found above." >&2
  echo "Define the colour in crates/observation-client/src/theme.rs and reference it from there." >&2
fi
exit "${status}"
