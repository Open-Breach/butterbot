#!/usr/bin/env bash
# Rehearses a release: runs publish-image.sh against a throwaway registry
# that requires a login, and checks what it leaves there.
#
#   rehearse-release.sh <archive for linux/amd64> <archive for linux/arm64>
#
# The archives are the tested images of the change. The registry is a
# container on this machine with a login made up here; it is removed at the
# end. No secret of the repository is used and nothing is attested.
#
# The images of the registry and of the tool that writes its password file
# can be replaced through REHEARSAL_REGISTRY_IMAGE and
# REHEARSAL_HTPASSWD_IMAGE, for a machine that reaches another mirror.
set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ "$#" -ne 2 ]; then
  echo "usage: rehearse-release.sh <archive for linux/amd64> <archive for linux/arm64>" >&2
  exit 1
fi
amd64="$1"
arm64="$2"

registry_image="${REHEARSAL_REGISTRY_IMAGE:-registry:3@sha256:82f6ff4a7f982960c8aa51617fea3aabc485df6e55cce80d22deb1de36f1bc3c}"
htpasswd_image="${REHEARSAL_HTPASSWD_IMAGE:-httpd:2.4-alpine@sha256:32bd0639bcc4917ef1fd9363a1d9dd2dd9c498565118b062a10d51e43a5c3a24}"

work="$(mktemp -d)"
container="butterbot-rehearsal-$$"
cleanup() {
  docker rm --force "$container" > /dev/null 2>&1 || true
  docker image rm --force "butterbot-rehearsal-other:amd64" "butterbot-rehearsal-other:arm64" > /dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

checks=0
step() {
  echo
  echo "== $*"
}

# Fails the rehearsal, with the output of the last run of the script.
wrong() {
  echo "rehearsal: WRONG: $*" >&2
  if [ -f "$work/own" ]; then
    echo "--- what the script wrote" >&2
    cat "$work/own" >&2
    echo "--- what the tools wrote" >&2
    cat "$work/tools" >&2
  fi
  exit 1
}

# equal <what> <actual> <expected>
equal() {
  checks=$((checks + 1))
  if [ "$2" != "$3" ]; then
    wrong "$1: is '$2', expected '$3'"
  fi
  echo "ok: $1"
}

# contains <what> <file> <text>
contains() {
  checks=$((checks + 1))
  if ! grep --quiet --fixed-strings -- "$3" "$2"; then
    wrong "$1: '$3' is not in $(basename "$2")"
  fi
  echo "ok: $1"
}

# lacks <what> <file> <text>
lacks() {
  checks=$((checks + 1))
  if grep --quiet --fixed-strings -- "$3" "$2"; then
    wrong "$1: '$3' is in $(basename "$2")"
  fi
  echo "ok: $1"
}

# --- The throwaway registry -----------------------------------------------------

step "A registry that requires a login"

user="rehearsal"
password="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
mkdir "$work/auth"
if command -v htpasswd > /dev/null; then
  htpasswd -Bbn "$user" "$password" > "$work/auth/htpasswd"
else
  docker run --rm --entrypoint htpasswd "$htpasswd_image" -Bbn "$user" "$password" > "$work/auth/htpasswd"
fi
chmod -R a+rX "$work/auth"

docker run --detach --name "$container" \
  --publish 127.0.0.1::5000 \
  --volume "$work/auth:/auth:ro" \
  --env REGISTRY_AUTH=htpasswd \
  --env REGISTRY_AUTH_HTPASSWD_REALM=rehearsal \
  --env REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd \
  "$registry_image" > /dev/null
port="$(docker port "$container" 5000/tcp | head -n 1 | sed 's/.*://')"
# Docker and its tools talk plain HTTP to a registry on localhost.
host="localhost:$port"
image="$host/butterbot"

for _ in $(seq 1 50); do
  code="$(curl --silent --output /dev/null --write-out '%{http_code}' "http://$host/v2/" || true)"
  [ "$code" = "401" ] && break
  sleep 0.2
done
equal "the registry refuses a request without a login" "$code" "401"

# What the registry holds, read with the login.
api() {
  curl --silent --show-error --fail --user "$user:$password" \
    --header 'Accept: application/vnd.oci.image.index.v1+json' \
    --header 'Accept: application/vnd.docker.distribution.manifest.list.v2+json' \
    --header 'Accept: application/vnd.oci.image.manifest.v1+json' \
    --header 'Accept: application/vnd.docker.distribution.manifest.v2+json' \
    "http://$host/v2/butterbot/$1"
}

# The digest a tag names, or `none`.
digest_of_tag() {
  local manifest
  if manifest="$(api "manifests/$1" 2> /dev/null)"; then
    printf 'sha256:%s' "$(printf '%s' "$manifest" | sha256sum | cut -d ' ' -f 1)"
  else
    printf 'none'
  fi
}

# Every tag with its digest, sorted: the state of the registry.
state() {
  local tag
  for tag in $(api "tags/list" 2> /dev/null | jq --raw-output '.tags // [] | sort | .[]'); do
    echo "$tag $(digest_of_tag "$tag")"
  done
}

# The identifier of the image a tag yields for a platform.
identifier_for() {
  local entry
  entry="$(api "manifests/$1" | jq --raw-output --arg arch "$2" \
    '.manifests[] | select(.platform.os == "linux" and .platform.architecture == $arch) | .digest')"
  api "manifests/$entry" | jq --raw-output '.config.digest'
}

# publish <version> <amd64 archive> <arm64 archive>, with the registry's
# settings unless the caller's environment says otherwise. What the script
# writes itself goes to $work/own, what its tools write to $work/tools.
status=0
publish() {
  : > "$work/summary"
  : > "$work/output"
  status=0
  REGISTRY_HOST="${REGISTRY_HOST-$host}" \
    REGISTRY_USERNAME="${REGISTRY_USERNAME-$user}" \
    REGISTRY_PASSWORD="${REGISTRY_PASSWORD-$password}" \
    GITHUB_STEP_SUMMARY="$work/summary" \
    GITHUB_OUTPUT="$work/output" \
    "$scripts/publish-image.sh" "$@" > "$work/own" 2>&1 3> "$work/tools" || status=$?
}

id_amd64="$("$scripts/image-id.sh" "$amd64")"
id_arm64="$("$scripts/image-id.sh" "$arm64")"

# Another build: images that differ from the tested ones, one per platform.
# They are imported, not built, so nothing has to run on a foreign platform.
echo "another build" > "$work/marker"
for platform in amd64 arm64; do
  tar --create --file - --directory "$work" marker \
    | docker import --platform "linux/$platform" - "butterbot-rehearsal-other:$platform" > /dev/null
  docker save --output "$work/other-$platform.tar" "butterbot-rehearsal-other:$platform"
done

# --- A setting is missing ---------------------------------------------------------

step "A missing setting stops the release before anything is pushed"

REGISTRY_PASSWORD="" publish 0.1.0 "$amd64" "$arm64"
equal "without REGISTRY_PASSWORD the script fails" "$status" "1"
contains "the message names REGISTRY_PASSWORD" "$work/own" "REGISTRY_PASSWORD"
equal "the registry is empty" "$(state)" ""

# --- A platform is missing --------------------------------------------------------

step "A missing platform sets no tag"

publish 0.1.0 "$amd64" "$work/no-such-archive.tar"
equal "without the archive for linux/arm64 the script fails" "$status" "1"
contains "the message names the platform" "$work/own" "linux/arm64"
equal "the registry is empty" "$(state)" ""

publish 0.1.0 "$arm64" "$amd64"
equal "with the archives the wrong way round the script fails" "$status" "1"
equal "the registry is empty" "$(state)" ""

# --- A wrong login ----------------------------------------------------------------

step "A refused login publishes nothing"

REGISTRY_PASSWORD="not-the-password" publish 0.1.0 "$amd64" "$arm64"
equal "with a wrong password the script fails" "$status" "1"
contains "the message says so" "$work/own" "refused the login"
equal "the registry is empty" "$(state)" ""

# --- A release --------------------------------------------------------------------

step "A release sets its three tags on one image for both platforms"

publish 0.1.0 "$amd64" "$arm64"
equal "the release succeeds" "$status" "0"
released="$(digest_of_tag 0.1.0)"
equal "0.1 names the image of 0.1.0" "$(digest_of_tag 0.1)" "$released"
equal "latest names the image of 0.1.0" "$(digest_of_tag latest)" "$released"
equal "the image lists exactly linux/amd64 and linux/arm64" \
  "$(api manifests/0.1.0 | jq --compact-output '[.manifests[] | select(.platform.os != "unknown") | "\(.platform.os)/\(.platform.architecture)"] | sort')" \
  '["linux/amd64","linux/arm64"]'
equal "for linux/amd64 it yields the tested image" "$(identifier_for 0.1.0 amd64)" "$id_amd64"
equal "for linux/arm64 it yields the tested image" "$(identifier_for 0.1.0 arm64)" "$id_arm64"
# The rehearsal itself reads the registry as a consumer would.
login() {
  docker login "$host" --username "$user" --password-stdin <<< "$password" > /dev/null 2>&1
}
login
for platform in amd64 arm64; do
  docker pull --quiet --platform "linux/$platform" "$image:0.1.0" > /dev/null
  equal "a pull for linux/$platform receives an image for linux/$platform" \
    "$(docker image inspect --platform "linux/$platform" --format '{{.Os}}/{{.Architecture}}' "$image:0.1.0")" "linux/$platform"
  docker image rm --force "$image:0.1.0" > /dev/null
done
docker logout "$host" > /dev/null 2>&1

step "The run reports what it published, and never names the registry itself"

contains "the summary has the version" "$work/summary" "| Version | \`0.1.0\` |"
contains "the summary has the tags" "$work/summary" "| Tags | \`0.1.0\`, \`0.1\`, \`latest\` |"
contains "the summary has the digest" "$work/summary" "| Digest | \`$released\` |"
contains "the summary names the image as butterbot" "$work/summary" "| Image | \`butterbot\` |"
equal "the digest is handed on" "$(cat "$work/output")" "digest=$released"
contains "the tools did name the registry, so the check below means something" "$work/tools" "$host"
for file in own summary output; do
  lacks "what the script wrote ($file) does not contain the registry's host" "$work/$file" "$host"
  lacks "nor the login" "$work/$file" "$user"
  lacks "nor the password" "$work/$file" "$password"
done
lacks "the tools did not print the password" "$work/tools" "$password"

before="$(state)"

# --- The job is run again ---------------------------------------------------------

step "Running the release again with the same images completes it"

publish 0.1.0 "$amd64" "$arm64"
equal "the second run succeeds" "$status" "0"
contains "it says that the version is there" "$work/own" "already in the registry with these images"
equal "the registry is unchanged" "$(state)" "$before"
equal "it reports the same digest" "$(cat "$work/output")" "digest=$released"

# A release that stopped after it had set the tag of the version: the tag is
# there, the other tags are not yet.
login
docker buildx imagetools create --tag "$image:1.0.0" "$image:0.1.0" > /dev/null 2>&1
docker logout "$host" > /dev/null 2>&1
equal "a half-finished release 1.0.0 is prepared" "$(digest_of_tag 1.0.0) $(digest_of_tag 1.0)" "$released none"
publish 1.0.0 "$amd64" "$arm64"
equal "running it again succeeds" "$status" "0"
equal "1.0.0 still names the same image" "$(digest_of_tag 1.0.0)" "$released"
equal "1.0 is set now" "$(digest_of_tag 1.0)" "$released"
equal "latest names it" "$(digest_of_tag latest)" "$released"

before="$(state)"

# --- Another build of a released version ------------------------------------------

step "Another build of a released version is refused"

publish 0.1.0 "$work/other-amd64.tar" "$work/other-arm64.tar"
equal "the script fails" "$status" "1"
contains "the message says that the version exists" "$work/own" "0.1.0 is already in the registry"
equal "the registry is unchanged" "$(state)" "$before"

publish 0.1.0 "$amd64" "$work/other-arm64.tar"
equal "one differing platform is enough" "$status" "1"
equal "the registry is unchanged" "$(state)" "$before"

# --- A pre-release ----------------------------------------------------------------

step "A pre-release sets only its own tag"

publish 0.2.0-rc.1 "$work/other-amd64.tar" "$work/other-arm64.tar"
equal "the pre-release succeeds" "$status" "0"
candidate="$(digest_of_tag 0.2.0-rc.1)"
checks=$((checks + 1))
[ "$candidate" != "none" ] && [ "$candidate" != "$released" ] || wrong "0.2.0-rc.1 names $candidate"
echo "ok: 0.2.0-rc.1 names an image of its own"
equal "latest still names the release before" "$(digest_of_tag latest)" "$released"
equal "0.1 still names the release before" "$(digest_of_tag 0.1)" "$released"
equal "there is no tag 0.2" "$(digest_of_tag 0.2)" "none"
contains "the summary has only that tag" "$work/summary" "| Tags | \`0.2.0-rc.1\` |"

before="$(state)"

# --- A platform is missing, with releases in the registry -------------------------

step "A release that lacks a platform moves no tag"

publish 0.3.0 "$amd64" "$work/no-such-archive.tar"
equal "the script fails" "$status" "1"
equal "the registry is unchanged" "$(state)" "$before"

echo
echo "rehearsal: $checks checks passed"
