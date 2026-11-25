#!/usr/bin/env bash
# The rehearsal of a release: runs the publishing steps (publish-image.sh)
# against a registry that exists only for this run, and checks what they
# leave there. It uses no secret: the registry requires a login, and the
# login is made up here.
#
#   rehearse-publish.sh <archive of linux/amd64> <archive of linux/arm64>
#
# The archives are the ones the image jobs wrote with `docker save`. What is
# checked is read from the registry over its HTTP interface with curl, not
# with the tools the publishing steps use.
#
# The comments name the scenarios of the spec `image-release` they check.

# The backquotes in single quotes are Markdown.
# shellcheck disable=SC2016
set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
amd64="${1:?usage: rehearse-publish.sh <archive of linux/amd64> <archive of linux/arm64>}"
arm64="${2:?usage: rehearse-publish.sh <archive of linux/amd64> <archive of linux/arm64>}"

# The registry of the Distribution project, by tag and by digest.
registry_image="registry:3.1.2@sha256:82f6ff4a7f982960c8aa51617fea3aabc485df6e55cce80d22deb1de36f1bc3c"

work="$(mktemp -d)"
container="butterbot-rehearsal-$$"
host=""
other_image="butterbot:rehearsal-another-build-$$"
cleanup() {
  docker rm --force "$container" > /dev/null 2>&1 || true
  if [ -n "$host" ]; then
    docker image ls --format '{{.Repository}}:{{.Tag}}' "$host/butterbot" 2> /dev/null \
      | while read -r reference; do docker image rm "$reference" > /dev/null 2>&1 || true; done
  fi
  docker image rm "$other_image" > /dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

failures=0
ok() {
  printf '  ok: %s\n' "$1"
}
not_ok() {
  printf '  FAILED: %s\n' "$1"
  failures=$((failures + 1))
}
# check <what> <command...>: the command must succeed.
check() {
  local what="$1"
  shift
  if "$@"; then ok "$what"; else not_ok "$what"; fi
}
# same <what> <actual> <expected>
same() {
  if [ "$2" = "$3" ]; then
    ok "$1"
  else
    not_ok "$1"
    printf '      expected: %s\n      actual:   %s\n' "$3" "$2"
  fi
}

# --- The registry -----------------------------------------------------------

user="rehearsal-$(openssl rand -hex 4)"
password="$(openssl rand -hex 24)"
if command -v htpasswd > /dev/null; then
  htpasswd -Bbn "$user" "$password" > "$work/htpasswd"
else
  docker run --rm --entrypoint htpasswd httpd:2.4-alpine -Bbn "$user" "$password" > "$work/htpasswd"
fi
docker create --name "$container" --publish 127.0.0.1::5000 \
  --env REGISTRY_AUTH=htpasswd \
  --env REGISTRY_AUTH_HTPASSWD_REALM=rehearsal \
  --env REGISTRY_AUTH_HTPASSWD_PATH=/htpasswd \
  "$registry_image" > /dev/null
docker cp "$work/htpasswd" "$container:/htpasswd" > /dev/null
docker start "$container" > /dev/null
port="$(docker port "$container" 5000/tcp | sed -n '1s/.*://p')"
# Docker and buildx talk plain HTTP to a registry on localhost.
host="localhost:$port"
api="http://$host/v2"
manifests="application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json"

status_without_login() {
  curl --silent --output /dev/null --write-out '%{http_code}' "$api/"
}
for _ in $(seq 1 30); do
  [ "$(status_without_login)" = "401" ] && break
  sleep 1
done

# The tags of the image, sorted, on one line.
tags() {
  local body
  body="$(curl --silent --user "$user:$password" "$api/butterbot/tags/list")"
  jq --raw-output '(.tags // []) | sort | join(" ")' <<< "$body"
}
# The manifest under a tag or a digest.
manifest() {
  curl --silent --fail --user "$user:$password" --header "Accept: $manifests" "$api/butterbot/manifests/$1"
}
# The digest of the manifest under a tag; empty when there is none.
digest() {
  curl --silent --head --user "$user:$password" --header "Accept: $manifests" "$api/butterbot/manifests/$1" \
    | tr -d '\r' | sed -n 's/^[Dd]ocker-[Cc]ontent-[Dd]igest: //p'
}
# The platforms a tag lists, sorted, on one line.
platforms() {
  manifest "$1" | jq --raw-output '[.manifests[] | "\(.platform.os)/\(.platform.architecture)"] | sort | join(" ")'
}
# The identifier of the image a tag yields for a platform.
identifier() {
  local for_platform
  for_platform="$(manifest "$1" | jq --raw-output --arg platform "$2" \
    '.manifests[] | select("\(.platform.os)/\(.platform.architecture)" == $platform) | .digest')"
  manifest "$for_platform" | jq --raw-output '.config.digest'
}
# Everything a later step must not have changed: each tag with its digest.
state() {
  local tag
  for tag in $(tags); do
    printf '%s=%s\n' "$tag" "$(digest "$tag")"
  done
}

# Runs the publishing steps. What the tools write goes to tools.log, what
# the script writes itself to own.log.
run=0
publish() {
  run=$((run + 1))
  out="$work/run-$run"
  mkdir "$out"
  : > "$out/summary.md"
  : > "$out/outputs"
  local status=0
  REGISTRY_HOST="$host" REGISTRY_USERNAME="$user" REGISTRY_PASSWORD="$password" \
    GITHUB_STEP_SUMMARY="$out/summary.md" GITHUB_OUTPUT="$out/outputs" \
    "$scripts/publish-image.sh" "$@" > "$out/own.log" 2>&1 3> "$out/tools.log" || status=$?
  sed 's/^/    | /' "$out/own.log"
  if [ "$status" -ne 0 ] && [ -s "$out/tools.log" ]; then
    echo "    the tools wrote:"
    tail -n 15 "$out/tools.log" | sed 's/^/    : /'
  fi
  return "$status"
}
succeeds() {
  if publish "$@"; then ok "the publishing steps succeed"; else not_ok "the publishing steps succeed"; fi
}
fails() {
  if publish "$@"; then not_ok "the publishing steps fail"; else ok "the publishing steps fail"; fi
}
says() {
  check "the message says \"$1\"" grep --quiet --fixed-strings -- "$1" "$out/own.log"
}

id_amd64="$("$scripts/image-id.sh" "$amd64")"
id_arm64="$("$scripts/image-id.sh" "$arm64")"
echo "The tested images: linux/amd64 $id_amd64, linux/arm64 $id_arm64."

# Another build of the image for the platform of this machine: the tested
# one with a label added, which gives it another identifier. It is built
# with the options the image job builds with, so that its manifest is of the
# same kind.
case "$(docker version --format '{{.Server.Arch}}')" in
  amd64) native="$amd64" ;;
  arm64 | aarch64) native="$arm64" ;;
  *) echo "the rehearsal runs on amd64 or arm64" >&2; exit 1 ;;
esac
loaded="$(docker load --input "$native" | sed -n 's/^Loaded image\( ID\)\{0,1\}: //p' | tail -n 1)"
mkdir "$work/another-build"
printf 'FROM %s\nLABEL rehearsal="another build"\n' "$loaded" > "$work/another-build/Dockerfile"
docker build --provenance=false --output type=docker,oci-mediatypes=false --quiet \
  --tag "$other_image" "$work/another-build" > /dev/null
docker save --output "$work/another-build.tar" "$other_image"
other_amd64="$amd64"
other_arm64="$arm64"
if [ "$native" = "$amd64" ]; then other_amd64="$work/another-build.tar"; else other_arm64="$work/another-build.tar"; fi
other_id_amd64="$("$scripts/image-id.sh" "$other_amd64")"
other_id_arm64="$("$scripts/image-id.sh" "$other_arm64")"

echo
echo "The registry requires a login (task 2.2)"
same "a request without a login is refused" "$(status_without_login)" "401"
same "the registry is empty" "$(tags)" ""

echo
echo "\"One platform is missing\": an archive is not there"
fails 0.1.0 "$amd64" "$work/no-such-archive.tar"
says "the image for linux/arm64 is missing"
same "nothing is in the registry" "$(tags)" ""

echo
echo "\"Release\", \"Tags of the platforms\", \"Both platforms listed\", \"Pulled for arm64\", \"The tested image is the published image\", \"Image name\""
succeeds 0.1.0 "$amd64" "$arm64"
same "the tags of the image butterbot" "$(tags)" "0.1 0.1.0 0.1.0-linux-amd64 0.1.0-linux-arm64 latest"
released="$(digest 0.1.0)"
check "the tag 0.1.0 names an image" test -n "$released"
same "0.1 names the same image as 0.1.0" "$(digest 0.1)" "$released"
same "latest names the same image as 0.1.0" "$(digest latest)" "$released"
same "0.1.0 lists exactly two platforms" "$(platforms 0.1.0)" "linux/amd64 linux/arm64"
same "for linux/amd64 it yields the tested image" "$(identifier 0.1.0 linux/amd64)" "$id_amd64"
same "for linux/arm64 it yields the tested image" "$(identifier 0.1.0 linux/arm64)" "$id_arm64"
printf '%s' "$password" | docker login "$host" --username "$user" --password-stdin > /dev/null 2>&1
for platform in linux/amd64 linux/arm64; do
  if docker pull --quiet --platform "$platform" "$host/butterbot:0.1.0" > /dev/null \
    && docker save --platform "$platform" --output "$work/pulled.tar" "$host/butterbot:0.1.0"; then
    same "docker pull for $platform receives an image for $platform" "$("$scripts/image-id.sh" --platform "$work/pulled.tar")" "$platform"
    same "and it is the tested image" "$("$scripts/image-id.sh" "$work/pulled.tar")" "$("$scripts/image-id.sh" "$([ "$platform" = linux/amd64 ] && echo "$amd64" || echo "$arm64")")"
  else
    not_ok "docker pull for $platform"
  fi
done
docker logout "$host" > /dev/null

echo
echo "\"Summary of a release\", \"Run summary\" (task 2.7)"
summary="$out/summary.md"
check "the summary names the image as butterbot" grep --quiet --fixed-strings '| Image | `butterbot` |' "$summary"
check "the summary shows the version" grep --quiet --fixed-strings '| Version | `0.1.0` |' "$summary"
check "the summary shows the tags" grep --quiet --fixed-strings '| Tags | `0.1.0`, `0.1`, `latest` |' "$summary"
check "the summary shows the digest of the multi-platform image" grep --quiet --fixed-strings "| Digest | \`$released\` |" "$summary"
same "the summary shows one digest" "$(grep --only-matching 'sha256:[0-9a-f]*' "$summary" | sort --unique | wc -l | tr -d ' ')" "1"
same "the digest is handed to later steps" "$(cat "$out/outputs")" "digest=$released"

echo
echo "\"Output of the publishing steps\": nothing the script writes itself names the registry or the login"
for file in own.log summary.md outputs; do
  for secret in "$host" "$user" "$password"; do
    if grep --quiet --fixed-strings -- "$secret" "$out/$file"; then
      not_ok "$file contains a registry setting"
    fi
  done
  ok "$file was searched for the host, the user and the password"
done
check "the tools did name the registry, so the search can find it" grep --quiet --fixed-strings -- "$host" "$out/tools.log"

echo
echo "\"Failed job run again\": the same images a second time"
before="$(state)"
succeeds 0.1.0 "$amd64" "$arm64"
says "is in the registry already, with exactly these images"
same "the tags name the same images as before" "$(state)" "$before"

echo
echo "\"Version already released from another build\""
fails 0.1.0 "$other_amd64" "$other_arm64"
says "version 0.1.0 exists in the registry already"
same "the registry is unchanged" "$(state)" "$before"

echo
echo "\"Pre-release\": 0.2.0-rc.1 after 0.1.0, from another build"
succeeds 0.2.0-rc.1 "$other_amd64" "$other_arm64"
same "only the tags of 0.2.0-rc.1 were added" "$(tags)" "0.1 0.1.0 0.1.0-linux-amd64 0.1.0-linux-arm64 0.2.0-rc.1 0.2.0-rc.1-linux-amd64 0.2.0-rc.1-linux-arm64 latest"
check "0.2.0-rc.1 names another image than 0.1.0" test "$(digest 0.2.0-rc.1)" != "$released"
same "0.2.0-rc.1 yields the other build for linux/amd64" "$(identifier 0.2.0-rc.1 linux/amd64)" "$other_id_amd64"
same "0.2.0-rc.1 yields the other build for linux/arm64" "$(identifier 0.2.0-rc.1 linux/arm64)" "$other_id_arm64"
same "latest still names 0.1.0" "$(digest latest)" "$released"
same "0.1 still names 0.1.0" "$(digest 0.1)" "$released"
check "the summary shows only the tag of the pre-release" grep --quiet --fixed-strings '| Tags | `0.2.0-rc.1` |' "$out/summary.md"

echo
echo "\"Release\": a second release, 0.2.0 after 0.1.0, from another build"
succeeds 0.2.0 "$other_amd64" "$other_arm64"
same "the tags of 0.2.0 were added" "$(tags)" "0.1 0.1.0 0.1.0-linux-amd64 0.1.0-linux-arm64 0.2 0.2.0 0.2.0-linux-amd64 0.2.0-linux-arm64 0.2.0-rc.1 0.2.0-rc.1-linux-amd64 0.2.0-rc.1-linux-arm64 latest"
second="$(digest 0.2.0)"
check "0.2.0 names another image than 0.1.0" test -n "$second" -a "$second" != "$released"
same "0.2 names 0.2.0" "$(digest 0.2)" "$second"
same "latest has moved to 0.2.0" "$(digest latest)" "$second"
same "0.1 and 0.1.0 still name 0.1.0" "$(digest 0.1) $(digest 0.1.0)" "$released $released"
same "0.2.0 lists exactly two platforms" "$(platforms 0.2.0)" "linux/amd64 linux/arm64"
same "0.2.0 yields the other build for linux/amd64" "$(identifier 0.2.0 linux/amd64)" "$other_id_amd64"
same "0.2.0 yields the other build for linux/arm64" "$(identifier 0.2.0 linux/arm64)" "$other_id_arm64"

echo
echo "\"One platform is missing\": the second image cannot be pushed"
before="$(state)"
mkdir "$work/bin"
real_docker="$(command -v docker)"
cat > "$work/bin/docker" << SHIM
#!/usr/bin/env bash
# The rehearsal's stand-in for a registry that refuses one image.
if [ "\${1:-}" = "push" ] && [[ "\${2:-}" == *-linux-arm64 ]]; then
  echo "the rehearsal refuses this push" >&2
  exit 1
fi
exec "$real_docker" "\$@"
SHIM
chmod +x "$work/bin/docker"
PATH="$work/bin:$PATH" fails 0.3.0 "$other_amd64" "$other_arm64"
says "the image for linux/arm64 could not be pushed"
same "only the first platform's own tag was added" "$(tags)" "0.1 0.1.0 0.1.0-linux-amd64 0.1.0-linux-arm64 0.2 0.2.0 0.2.0-linux-amd64 0.2.0-linux-arm64 0.2.0-rc.1 0.2.0-rc.1-linux-amd64 0.2.0-rc.1-linux-arm64 0.3.0-linux-amd64 latest"
same "latest and 0.2 were not moved" "$(digest latest) $(digest 0.2)" "$second $second"
same "no tag of 0.3.0 was set" "$(digest 0.3.0)$(digest 0.3)" ""

echo
echo "\"Version already released from another build\": the registry cannot say whether the version exists"
before="$(state)"
cat > "$work/bin/docker" << SHIM
#!/usr/bin/env bash
# The rehearsal's stand-in for a registry that answers with an error.
if [ "\${1:-} \${2:-} \${3:-} \${4:-}" = "buildx imagetools inspect --raw" ] && [[ "\${5:-}" == *:0.5.0 ]]; then
  echo "ERROR: unexpected status from the rehearsal: 500 Internal Server Error" >&2
  exit 1
fi
exec "$real_docker" "\$@"
SHIM
PATH="$work/bin:$PATH" fails 0.5.0 "$amd64" "$arm64"
says "the registry did not say whether version 0.5.0 exists"
same "nothing was pushed" "$(state)" "$before"

echo
echo "A tag that holds a manifest of another kind (not a scenario of its own: no tag of the release may be set)"
# What `latest` holds, put there again with the media type of the other
# kind. A registry may refuse to replace such a manifest, and the tool would
# find that out only after it has set the tag of the full version.
held="$(manifest latest)"
case "$(jq --raw-output '.mediaType' <<< "$held")" in
  application/vnd.docker.*) other_kind="application/vnd.oci.image.index.v1+json" ;;
  *) other_kind="application/vnd.docker.distribution.manifest.list.v2+json" ;;
esac
jq --compact-output --arg type "$other_kind" '.mediaType = $type' <<< "$held" > "$work/other-kind.json"
same "the registry takes latest as a manifest of the other kind" \
  "$(curl --silent --output /dev/null --write-out '%{http_code}' --user "$user:$password" --request PUT \
    --header "Content-Type: $other_kind" --data-binary "@$work/other-kind.json" "$api/butterbot/manifests/latest")" "201"
before="$(state)"
fails 0.4.0 "$amd64" "$arm64"
says "the tag latest holds a manifest of another kind"
same "no tag of 0.4.0 was set, and latest was not moved" "$(digest 0.4.0)$(digest 0.4) $(digest latest)" " $(sed -n 's/^latest=//p' <<< "$before")"

echo
echo "A login the registry does not know (not a scenario of its own: nothing may be pushed)"
status=0
REGISTRY_HOST="$host" REGISTRY_USERNAME="$user" REGISTRY_PASSWORD="not-the-password" \
  "$scripts/publish-image.sh" 0.6.0 "$amd64" "$arm64" > "$work/wrong-login.log" 2>&1 3> /dev/null || status=$?
same "the publishing steps fail" "$status" "1"
check "the message says that the login was refused" grep --quiet --fixed-strings "the registry refused the login" "$work/wrong-login.log"
same "no tag of 0.6.0 was set" "$(digest 0.6.0)" ""

echo
if [ "$failures" -ne 0 ]; then
  echo "The rehearsal failed: $failures checks."
  exit 1
fi
echo "The rehearsal passed."
