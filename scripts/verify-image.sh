#!/usr/bin/env bash
# Checks that a published image was signed (cosign, keyless) and attested (GitHub artifact
# attestation) by this repository's Release workflow running on the expected ref, and prints
# the image pinned by digest (IMAGE@sha256:...), so that what is then pulled is what was
# checked.
#
# Usage: scripts/verify-image.sh IMAGE [REF]
#   IMAGE  ghcr.io/panevain/observation or ghcr.io/panevain/observation-regions, with :TAG or
#          @sha256:DIGEST.
#   REF    The git ref Release must have run on. Defaults: refs/tags/vX.Y.Z for :X.Y.Z (and
#          :X.Y.Z-dev.N), refs/heads/develop for :devel, and any release tag for :latest or a
#          digest.
#
# The signer identity is pinned to the ref, not just to release.yml: Release runs from any ref
# whose copy of release.yml triggers it, so "signed by release.yml" alone would also accept an
# image published from a pushed branch. Only admins and the release bot can create v* tags
# (.github/rulesets/version-tags-create.json), and nobody can move them.
#
# Needs cosign (`mise run verify` installs it). The provenance attestation is checked too when
# gh is installed and logged in; set VERIFY_ATTESTATION=required to fail without it (Release
# does).
set -euo pipefail

repo="panevain/observation"
workflow="https://github.com/${repo}/.github/workflows/release.yml"
issuer="https://token.actions.githubusercontent.com"
semver='[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?'

image="${1:?usage: $0 IMAGE [REF]}"
ref="${2:-}"

name="${image%%@*}"
digest=""
if [[ "${image}" == *@* ]]; then digest="${image#*@}"; fi
tag=""
if [[ "${name##*/}" == *:* ]]; then tag="${name##*:}"; name="${name%:*}"; fi
case "${name}" in
  "ghcr.io/${repo}" | "ghcr.io/${repo}-regions") ;;
  *) echo "not an image this repository publishes: ${name}" >&2; exit 2 ;;
esac
if [[ -n "${digest}" && ! "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo "not a sha256 digest: ${digest}" >&2; exit 2
fi
if [[ -z "${tag}" && -z "${digest}" ]]; then tag=latest; fi

if [[ -z "${ref}" && -z "${digest}" ]]; then
  if [[ "${tag}" =~ ^${semver}$ ]]; then
    ref="refs/tags/v${tag}"
  elif [[ "${tag}" == devel ]]; then
    ref="refs/heads/develop"
  elif [[ "${tag}" != latest ]]; then
    echo "pass the ref that ${name}:${tag} was published from (refs/tags/vX.Y.Z)" >&2; exit 2
  fi
fi
if [[ -n "${ref}" && ! "${ref}" =~ ^refs/(tags/v${semver}|heads/develop)$ ]]; then
  echo "Release publishes only from refs/tags/vX.Y.Z and refs/heads/develop, not ${ref}" >&2; exit 2
fi

if [[ -n "${ref}" ]]; then
  identity=(--certificate-identity "${workflow}@${ref}")
  gh_identity=(--cert-identity "${workflow}@${ref}" --source-ref "${ref}")
  wanted="${ref}"
else
  # Any release tag, for :latest and bare digests.
  regex="^${workflow//./\\.}@refs/tags/v${semver}\$"
  identity=(--certificate-identity-regexp "${regex}")
  gh_identity=(--cert-identity-regex "${regex}")
  wanted="a release tag"
fi

if [[ -n "${digest}" ]]; then subject="${name}@${digest}"; else subject="${name}:${tag}"; fi

# cosign prints the verified payloads; the manifest digest in them is what the signature
# covers, so pinning to it closes the gap between this check and the pull.
errors="$(mktemp)"
trap 'rm -f "${errors}"' EXIT
payload="$(cosign verify "${identity[@]}" --certificate-oidc-issuer "${issuer}" "${subject}" 2>"${errors}")" || {
  echo "${subject}: no valid signature from ${workflow} on ${wanted}" >&2
  sed -n '/^Error: /{s/^Error: /  cosign: /p;q}' "${errors}" >&2
  exit 1
}
signed="$(grep -oE '"docker-manifest-digest": *"sha256:[0-9a-f]{64}"' <<<"${payload}" | grep -oE 'sha256:[0-9a-f]{64}' | sort -u)"
[[ "${signed}" =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "${subject}: could not read the one digest the signatures cover" >&2; exit 1; }
if [[ -n "${digest}" && "${signed}" != "${digest}" ]]; then
  echo "${subject}: the signature covers ${signed}, not ${digest}" >&2; exit 1
fi

# gh needs GitHub credentials (a login, or GH_TOKEN as in Actions) even though the bundle comes
# from the registry. `gh auth token` only reads the stored login; it makes no request.
gh_ready() {
  command -v gh >/dev/null 2>&1 &&
    { [[ -n "${GH_TOKEN:-}${GITHUB_TOKEN:-}" ]] || gh auth token >/dev/null 2>&1; }
}
if gh_ready; then
  if ! gh attestation verify "oci://${name}@${signed}" --repo "${repo}" --bundle-from-oci \
    "${gh_identity[@]}" --cert-oidc-issuer "${issuer}" --deny-self-hosted-runners >/dev/null; then
    echo "${name}@${signed}: no valid provenance attestation from ${workflow} on ${wanted}" >&2
    exit 1
  fi
  attested="attested"
elif [[ "${VERIFY_ATTESTATION:-}" == required ]]; then
  echo "a logged-in gh is needed to check the provenance attestation (VERIFY_ATTESTATION=required)" >&2
  exit 1
else
  attested="provenance not checked (needs gh, logged in)"
fi

echo "${subject}: signed by Release on ${wanted}, ${attested}" >&2
echo "${name}@${signed}"
