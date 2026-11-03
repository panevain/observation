#!/usr/bin/env bash
# The production host's end of Deploy (.github/workflows/deploy.yml, docs/deploy.md).
# scripts/deploy-host-setup.sh installs it as the forced command of the deploy key, so that key
# can do one thing: run a release of this repository, by the digests Release signed.
#
#   ssh observation-deploy@HOST deploy X.Y.Z [APP_DIGEST REGIONS_DIGEST]
#   sudo -u observation-deploy /usr/local/libexec/observation-deploy deploy X.Y.Z   # on the host
#
# It fetches tag vX.Y.Z (only admins and the release bot can create v* tags, and nobody can move
# them), checks both images with that tag's scripts/verify-image.sh (cosign; the signer must be
# release.yml on refs/tags/vX.Y.Z), starts that tag's compose.yaml with the images pinned by
# digest, and waits for /healthz. Without digests it uses the ones the X.Y.Z tags point at.
# Settings and secrets come from /etc/observation/observation.env and never leave the host.
set -euo pipefail
umask 077

repo_url="https://github.com/panevain/observation.git"
env_file="/etc/observation/observation.env"
# The deploy user's home (/var/lib/observation-deploy), whatever HOME a sudo left behind.
state="$(getent passwd "$(id -un)" | cut -d: -f6)"
[ -n "${state}" ] && [ -d "${state}" ] || { echo "no home directory for $(id -un)" >&2; exit 1; }
src="${state}/src"
app="ghcr.io/panevain/observation"
regions="${app}-regions"
digest='^sha256:[0-9a-f]{64}$'

# Over SSH the words come from the client's command line; only exact matches below get through.
read -r -a args <<<"${SSH_ORIGINAL_COMMAND:-$*}"
usage() { echo "usage: deploy X.Y.Z [APP_DIGEST REGIONS_DIGEST]" >&2; exit 2; }
{ [ "${#args[@]}" -eq 2 ] || [ "${#args[@]}" -eq 4 ]; } && [ "${args[0]}" = deploy ] || usage
version="${args[1]}"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
if [ "${#args[@]}" -eq 4 ]; then
  [[ "${args[2]}" =~ ${digest} && "${args[3]}" =~ ${digest} ]] || usage
  app_ref="${app}@${args[2]}"
  regions_ref="${regions}@${args[3]}"
else
  app_ref="${app}:${version}"
  regions_ref="${regions}:${version}"
fi
[ -r "${env_file}" ] || { echo "${env_file} is missing (scripts/deploy-host-setup.sh writes it)" >&2; exit 1; }

exec 9>"${state}/deploy.lock"
flock -w 600 9 || { echo "another deploy is still running" >&2; exit 1; }

# The release's own compose.yaml and verify-image.sh, from the tag (not a branch of that name).
tag="refs/tags/v${version}"
if [ ! -d "${src}/.git" ]; then
  git init -q "${src}"
  git -C "${src}" remote add origin "${repo_url}"
fi
git -C "${src}" fetch -q --depth 1 --no-tags origin "+${tag}:${tag}"
git -C "${src}" -c advice.detachedHead=false checkout -q --force --detach "${tag}"
git -C "${src}" clean -qfdx

OBS_IMAGE="$("${src}/scripts/verify-image.sh" "${app_ref}" "${tag}")"
OBS_REGIONS_IMAGE="$("${src}/scripts/verify-image.sh" "${regions_ref}" "${tag}")"
OBS_VERSION="${version}"
export OBS_IMAGE OBS_REGIONS_IMAGE OBS_VERSION

# Variables set here win over the env file. --no-build: compose.yaml can also build from source.
compose=(docker compose --project-name observation --project-directory "${src}"
  --file "${src}/compose.yaml" --env-file "${env_file}")
"${compose[@]}" up --detach --pull missing --no-build --remove-orphans

port="$("${compose[@]}" port observation 8080 | head -n 1)"
port="${port##*:}"
[ -n "${port}" ] || { echo "the app publishes no host port" >&2; exit 1; }
deadline=$((SECONDS + 60))
until curl -fsS --max-time 2 -o /dev/null "http://127.0.0.1:${port}/healthz"; do
  if (( SECONDS >= deadline )); then
    echo "v${version} did not pass /healthz within 60 s" >&2
    "${compose[@]}" ps >&2 || true
    "${compose[@]}" logs --no-color --tail 50 >&2 || true
    exit 1
  fi
  sleep 2
done

printf '%s\n%s\n%s\n' "v${version}" "${OBS_IMAGE}" "${OBS_REGIONS_IMAGE}" > "${state}/deployed"
# Unused, untagged images of this repository (earlier releases); nothing else on the host.
docker image prune --force --filter "label=org.opencontainers.image.source=https://github.com/panevain/observation" >/dev/null || true
echo "v${version} is up: ${OBS_IMAGE}, ${OBS_REGIONS_IMAGE}"
