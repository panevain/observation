#!/usr/bin/env bash
# Compares the rulesets in .github/rulesets/ with the repository's live ones, or applies them.
# Rulesets are matched by name.
#   check          diff every file against the live ruleset; also lists live rulesets that
#                  have no file. Exits 1 on any difference.
#   apply          create or update the live ruleset of every file (repository admin).
#   apply --prune  also delete live rulesets that have no file.
# Needs gh (authenticated) and jq. GH_REPO overrides the repository (default: this checkout's).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cmd="${1:?usage: $0 check | apply [--prune]}"
prune="${2:-}"
repo="${GH_REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"

# Fields GitHub adds to a ruleset that are not part of its definition.
normalize='del(.id, .source, .source_type, .node_id, ._links, .created_at, .updated_at, .current_user_can_bypass)
  | .rules |= map(if .parameters then .parameters |= with_entries(select(.value != null)) else . end)'

live="$(gh api "repos/${repo}/rulesets?includes_parents=false" --paginate --jq '.[] | [.id, .name] | @tsv')"
id_of() { awk -F'\t' -v n="$1" '$2 == n { print $1 }' <<<"${live}"; }

status=0
declare -A have
for f in .github/rulesets/*.json; do
  name="$(jq -r .name "${f}")"
  have["${name}"]=1
  id="$(id_of "${name}")"
  case "${cmd}" in
    check)
      if [[ -z "${id}" ]]; then
        echo "missing: ${name} (${f}) is not applied"
        status=1
      elif ! diff -u --label "live: ${name}" --label "${f}" \
        <(gh api "repos/${repo}/rulesets/${id}" | jq -S "${normalize}") <(jq -S . "${f}"); then
        status=1
      else
        echo "in sync: ${name}"
      fi
      ;;
    apply)
      if [[ -z "${id}" ]]; then
        gh api -X POST "repos/${repo}/rulesets" --input "${f}" --jq '"created: \(.name) (\(.id))"'
      else
        gh api -X PUT "repos/${repo}/rulesets/${id}" --input "${f}" --jq '"updated: \(.name) (\(.id))"'
      fi
      ;;
    *) echo "unknown command ${cmd}" >&2; exit 2 ;;
  esac
done

while IFS=$'\t' read -r id name; do
  [[ -n "${id}" && -z "${have[${name}]:-}" ]] || continue
  if [[ "${cmd}" == apply && "${prune}" == --prune ]]; then
    gh api -X DELETE "repos/${repo}/rulesets/${id}" && echo "deleted: ${name} (${id})"
  else
    echo "extra: live ruleset ${name} (${id}) has no file in .github/rulesets/"
    [[ "${cmd}" == check ]] && status=1
  fi
done <<<"${live}"

exit "${status}"
