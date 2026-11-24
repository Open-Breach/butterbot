#!/usr/bin/env bash
# Publishes the two tested images of a release as one multi-platform image
# `<REGISTRY_HOST>/butterbot`. The release workflow runs this against the
# real registry, and the rehearsal in the CI workflow runs the same script
# against a throwaway one (rehearse-publish.sh).
#
#   REGISTRY_HOST=... REGISTRY_USERNAME=... REGISTRY_PASSWORD=... \
#     publish-image.sh <version> <archive of linux/amd64> <archive of linux/arm64>
#
# The archives are the ones the image jobs wrote with `docker save`.
#
# In order:
#   1. Fail if a setting is missing, naming it.
#   2. Log in.
#   3. Read each archive's image identifier and load the archive.
#   4. If the registry holds the tag of the full version: fail when it names
#      other images than these, having pushed nothing; go on when it names
#      exactly these.
#   5. Push the image of each platform under a tag of its own,
#      <version>-linux-amd64 and <version>-linux-arm64. `docker push` cannot
#      push an image without a tag.
#   6. Create the multi-platform image from the two and set the tags:
#      X.Y.Z, X.Y and latest, or only the full version for a pre-release.
#   7. Write the version, the tags and the digest to the run summary, and
#      the digest to the step's outputs.
#
# The registry's host and the login are secrets. This script hands them to
# the tools as they are and never writes them itself: not to its output, not
# to the summary, not to the outputs. It does not transform the host either,
# because the platform masks only the exact value in the log. Do not add
# `set -x`.
#
# What the tools write (docker, buildx) goes to file descriptor 3, which is
# the standard error unless the caller opens it. The rehearsal opens it, to
# check that what is left, the script's own output, does not name the host.
set -euo pipefail

name="butterbot"
scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=.github/scripts/version.sh
. "$scripts/version.sh"

if ! { true >&3; } 2> /dev/null; then
  exec 3>&2
fi

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'publish-image: %s\n' "$*" >&2
  exit 1
}

# 1. The settings. Nothing is contacted before all three are there.
missing=0
for setting in REGISTRY_HOST REGISTRY_USERNAME REGISTRY_PASSWORD; do
  if [ -z "${!setting:-}" ]; then
    printf 'publish-image: %s is missing or empty. It is one of the three secrets of the environment "release".\n' "$setting" >&2
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  exit 2
fi

if [ "$#" -ne 3 ]; then
  echo "usage: publish-image.sh <version> <archive of linux/amd64> <archive of linux/arm64>" >&2
  exit 2
fi
version="$1"
platforms=("linux/amd64" "linux/arm64")
archives=("$2" "$3")

if ! [[ "$version" =~ $semantic_version ]]; then
  printf 'publish-image: "%s" is not a version of the form X.Y.Z or X.Y.Z-<suffix>\n' "$version" >&2
  exit 2
fi
tags=("$version")
if [ -z "${BASH_REMATCH[4]}" ]; then
  tags+=("${BASH_REMATCH[1]}.${BASH_REMATCH[2]}" "latest")
fi
# The tag of such a version would be the tag of one platform's image of
# another version.
case "$version" in
  *-linux-amd64 | *-linux-arm64)
    printf 'publish-image: a version must not end in -linux-amd64 or -linux-arm64: "%s"\n' "$version" >&2
    exit 2
    ;;
esac

image="$REGISTRY_HOST/$name"
work="$(mktemp -d)"
logged_in=0
finish() {
  if [ "$logged_in" -eq 1 ]; then
    docker logout "$REGISTRY_HOST" >&3 2>&3 || true
  fi
  rm -rf "$work"
}
trap finish EXIT

# The manifest the registry holds under a reference, as the registry sent
# it. Returns 1 when the registry has no such manifest, and fails when the
# registry could not be asked: an answer that was not understood must never
# be taken for "not there".
manifest_of() {
  if docker buildx imagetools inspect --raw "$1" 2> "$work/inspect-error"; then
    return 0
  fi
  cat "$work/inspect-error" >&3
  if grep -q -i 'not found' "$work/inspect-error"; then
    return 1
  fi
  fail "the registry did not answer the question for a manifest; see the output of the tool above"
}

# The digest of the manifest the registry holds under a reference.
digest_of() {
  docker buildx imagetools inspect --format '{{json .Manifest}}' "$1" 2>&3 | jq --raw-output '.digest'
}

# The images a multi-platform manifest names, one line per image:
# `<os>/<architecture> <image identifier>`, sorted. A manifest that is a
# single image gives one line without a platform.
images_of() {
  local manifest="$1" platform digest
  if [ "$(jq 'has("manifests")' <<< "$manifest")" != "true" ]; then
    printf 'a single image %s\n' "$(jq --raw-output '.config.digest' <<< "$manifest")"
    return 0
  fi
  jq --raw-output '.manifests[] | "\(.platform.os)/\(.platform.architecture) \(.digest)"' <<< "$manifest" \
    | while read -r platform digest; do
      printf '%s %s\n' "$platform" "$(manifest_of "$image@$digest" | jq --raw-output '.config.digest')"
    done | sort
}

# 2. Log in.
say "Logging in to the registry."
printf '%s' "$REGISTRY_PASSWORD" \
  | docker login "$REGISTRY_HOST" --username "$REGISTRY_USERNAME" --password-stdin >&3 2>&3 \
  || fail "the registry refused the login"
logged_in=1

# 3. The two images. Both must be there before anything is pushed.
for index in 0 1; do
  if [ ! -f "${archives[$index]}" ]; then
    fail "the image for ${platforms[$index]} is missing: there is no archive at ${archives[$index]}. Nothing was pushed."
  fi
done
identifiers=()
local_names=()
expected=""
for index in 0 1; do
  platform="${platforms[$index]}"
  archive="${archives[$index]}"
  identifier="$("$scripts/image-id.sh" "$archive")" \
    || fail "the archive for $platform could not be read. Nothing was pushed."
  built_for="$("$scripts/image-id.sh" --platform "$archive")"
  if [ "$built_for" != "$platform" ]; then
    fail "the archive given for $platform holds an image for $built_for. Nothing was pushed."
  fi
  # `docker load` says which name or ID the image has now. Two archives may
  # carry the same name, so the image gets a name of its own at once.
  loaded="$(docker load --input "$archive" 2>&3 | tee /dev/fd/3 | sed -n 's/^Loaded image\( ID\)\{0,1\}: //p' | tail -n 1)"
  if [ -z "$loaded" ]; then
    fail "the archive for $platform could not be loaded. Nothing was pushed."
  fi
  local_name="$image:$version-${platform/\//-}"
  docker tag "$loaded" "$local_name" >&3 2>&3
  identifiers+=("$identifier")
  local_names+=("$local_name")
  expected+="$platform $identifier"$'\n'
  say "The image for $platform is $identifier."
done
expected="$(printf '%s' "$expected" | sort)"

# 4. A released version is not overwritten. The same images are let through,
# so that a publishing job that failed half way can be run again.
if existing="$(manifest_of "$image:$version")"; then
  if [ "$(images_of "$existing")" != "$expected" ]; then
    fail "version $version exists in the registry already, and its tag names other images than the ones of this run. Nothing was pushed. Delete the tags of $version in the registry, or release the next version."
  fi
  say "Version $version is in the registry already, with exactly these images. Completing it."
else
  say "Version $version is not in the registry yet."
fi

# 5. The image of each platform, under a tag of its own.
sources=()
for index in 0 1; do
  platform="${platforms[$index]}"
  say "Pushing the image for $platform as $name:${local_names[$index]##*:}."
  docker push "${local_names[$index]}" >&3 2>&3 \
    || fail "the image for $platform could not be pushed. No tag of the release was set or moved."
  pushed="$(manifest_of "${local_names[$index]}")" \
    || fail "the image for $platform is not in the registry after the push. No tag of the release was set or moved."
  if [ "$(jq 'has("manifests")' <<< "$pushed")" = "true" ]; then
    fail "what was pushed for $platform is a list of manifests and not one image. The image was built with attestations attached; build it with --provenance=false. No tag of the release was set or moved."
  fi
  if [ "$(jq --raw-output '.config.digest' <<< "$pushed")" != "${identifiers[$index]}" ]; then
    fail "the registry holds another image for $platform than the one that was tested. No tag of the release was set or moved."
  fi
  sources+=("$image@$(digest_of "${local_names[$index]}")")
done

# 6. The multi-platform image, and the tags. They are set last.
say "Setting the tags: ${tags[*]}."
tag_arguments=()
for tag in "${tags[@]}"; do
  tag_arguments+=(--tag "$image:$tag")
done
docker buildx imagetools create "${tag_arguments[@]}" "${sources[@]}" >&3 2>&3 \
  || fail "the tags could not be set. Run the job again to complete the release."

digest="$(digest_of "$image:$version")"
for tag in "${tags[@]}"; do
  if [ "$(digest_of "$image:$tag")" != "$digest" ]; then
    fail "the tag $tag does not name the image of version $version"
  fi
done
published="$(manifest_of "$image:$version")"
if [ "$(images_of "$published")" != "$expected" ]; then
  fail "the tag $version does not name exactly the two tested images"
fi

# 7. What was published. The image is named without its registry.
listed=""
for tag in "${tags[@]}"; do
  listed+="${listed:+, }\`$tag\`"
done
# shellcheck disable=SC2016  # the backquotes are Markdown
{
  printf '### Published `%s` %s\n\n' "$name" "$version"
  printf '| | |\n|---|---|\n'
  printf '| Image | `%s` |\n' "$name"
  printf '| Version | `%s` |\n' "$version"
  printf '| Tags | %s |\n' "$listed"
  printf '| Platforms | `%s`, `%s` |\n' "${platforms[0]}" "${platforms[1]}"
  printf '| Digest | `%s` |\n' "$digest"
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
printf 'digest=%s\n' "$digest" >> "${GITHUB_OUTPUT:-/dev/null}"
say "Published $name $version as $digest."
