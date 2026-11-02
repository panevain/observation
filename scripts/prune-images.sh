#!/usr/bin/env bash
# Deletes the tagged versions of a GHCR container package that prune-images.yml does not keep:
# it keeps `devel`, `latest` and every X.Y.Z (or X.Y.Z-pre) tag, and deletes any other tagged
# version (old edge, main, X.Y and sha-* tags).
#
# `sha256-<digest>` tags are the registry's fallback for the build attestation of image
# <digest>. They are kept while the registry can still serve that image (including as an
# untagged version) and deleted only when the registry answers 404 for it, because nothing can
# pull or verify it any more. Any other answer (error, no registry token) keeps the tag.
#
# Untagged versions are never touched: the SBOM and provenance attestations are untagged
# children of a tagged image index, and deleting them breaks `docker pull` and attestation
# verification.
#
# Usage: GH_TOKEN=... REPO=OWNER/NAME scripts/prune-images.sh PACKAGE [--dry-run]
#   Does nothing while a Release run is in progress. --dry-run lists what would be deleted.
set -euo pipefail

package="${1:?usage: $0 PACKAGE [--dry-run]}"
dry_run=false
[[ "${2:-}" != --dry-run ]] || dry_run=true
: "${GH_TOKEN:?GH_TOKEN is required}" "${REPO:?REPO (OWNER/NAME) is required}"
owner="${REPO%%/*}"

keep_regex='^(devel|latest|[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)$'
attestation_regex='^sha256-([0-9a-f]{64})(\.[a-z]+)?$'

busy="$(gh run list --repo "${REPO}" --workflow release.yml --json status \
  --jq '[.[] | select(.status != "completed")] | length')"
if [ "${busy}" != 0 ]; then
  echo "A Release run is in progress; skipping."
  exit 0
fi
case "$(gh api "users/${owner}" --jq .type)" in
  Organization) base="orgs/${owner}" ;;
  *) base="users/${owner}" ;;
esac
url="${base}/packages/container/${package}/versions"
if ! gh api "${base}/packages/container/${package}" --silent 2>/dev/null; then
  echo "Package ${package} does not exist yet; nothing to prune."
  exit 0
fi

versions="$(mktemp)"
trap 'rm -f "${versions}"' EXIT
gh api --paginate "${url}?per_page=100" --jq '.[]' | jq -s '.' > "${versions}"
# Anonymous-style pull token for the registry; empty on failure, in which case no attestation
# tag is ever treated as orphaned.
reg_token="$(curl -fsS -u "${owner}:${GH_TOKEN}" \
  "https://ghcr.io/token?scope=repository:${owner,,}/${package}:pull" | jq -r '.token // empty' || true)"

# True only when the registry says 404 for the image with this digest.
image_gone() {
  [ -n "${reg_token}" ] || return 1
  local code
  code="$(curl -s -o /dev/null -I -w '%{http_code}' \
    -H "Authorization: Bearer ${reg_token}" \
    -H "Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json" \
    "https://ghcr.io/v2/${owner,,}/${package}/manifests/sha256:$1" || true)"
  [ "${code}" = 404 ]
}

# id <TAB> comma-separated tags, only for versions that have a tag and none of whose tags is on
# the keep list. An attestation tag is on the keep list unless the image it refers to is gone.
doomed="$(jq -r '.[] | select((.metadata.container.tags | length) > 0) | [.id, (.metadata.container.tags | join(","))] | @tsv' "${versions}" |
  while IFS=$'\t' read -r id tags; do
    keep=false
    IFS=, read -ra arr <<<"${tags}"
    for t in "${arr[@]}"; do
      if [[ "${t}" =~ ${keep_regex} ]]; then
        keep=true
      elif [[ "${t}" =~ ${attestation_regex} ]]; then
        if ! image_gone "${BASH_REMATCH[1]}"; then keep=true; fi
      fi
    done
    if [ "${keep}" = false ]; then printf '%s\t%s\n' "${id}" "${tags}"; fi
  done)"

if [ -z "${doomed}" ]; then
  echo "Nothing to delete."
  exit 0
fi
failed=0
while IFS=$'\t' read -r id tags; do
  if [ "${dry_run}" = true ]; then
    echo "Would delete version ${id} (tags: ${tags})"
  else
    echo "Deleting version ${id} (tags: ${tags})"
    gh api -X DELETE "${url}/${id}" || { echo "::warning::could not delete version ${id}"; failed=1; }
  fi
done <<<"${doomed}"
exit "${failed}"
