#!/usr/bin/env bash
# Publishes the tested images of a release to a registry, as one image for
# both platforms.
#
#   publish-image.sh <version> <archive for linux/amd64> <archive for linux/arm64>
#
# The archives are the ones `docker save` wrote in the image jobs. The
# registry is described by three variables of the environment:
#
#   REGISTRY_HOST      the host of the registry, as Docker names it
#   REGISTRY_USERNAME  the login
#   REGISTRY_PASSWORD  its password
#
# The image is published as <REGISTRY_HOST>/butterbot.
#
# The registry is a secret. This script never writes its host or the login:
# every message of its own names the image as `butterbot`. The tools it calls
# do write the host, so their output is kept apart, on file descriptor 3
# (standard error unless the caller opens it), and the script does nothing to
# the host that would change how it is spelled. Never run it with `set -x`.
#
# In order: settings, archives, login, the check of a version that exists,
# the image of each platform, then the image for both and the tags. The tags
# are set last, so a release that fails on the way moves none of them.
set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The output of the tools goes to file descriptor 3.
if ! { true >&3; } 2> /dev/null; then
  exec 3>&2
fi

say() {
  echo "publish-image: $*"
}

fail() {
  echo "publish-image: $*" >&2
  exit 1
}

# --- 1. Settings --------------------------------------------------------------

missing=()
for name in REGISTRY_HOST REGISTRY_USERNAME REGISTRY_PASSWORD; do
  if [ -z "${!name:-}" ]; then
    missing+=("$name")
  fi
done
if [ "${#missing[@]}" -gt 0 ]; then
  fail "${missing[*]} not set: the release environment needs REGISTRY_HOST, REGISTRY_USERNAME and REGISTRY_PASSWORD"
fi

if [ "$#" -ne 3 ]; then
  fail "usage: publish-image.sh <version> <archive for linux/amd64> <archive for linux/arm64>"
fi
version="$1"
declare -A archive=([amd64]="$2" [arm64]="$3")
platforms=(amd64 arm64)

if ! [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]]; then
  fail "$version is not a version such as 0.1.0 or 0.2.0-rc.1"
fi
major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
if [ -n "${BASH_REMATCH[4]}" ]; then
  # A pre-release moves neither `latest` nor the tag of its line.
  tags=("$version")
else
  tags=("$version" "$major.$minor" "latest")
fi

# --- 2. The archives ----------------------------------------------------------

# The identifier of an image is the digest of its configuration; see
# image-id.sh. It is the same in the archive and in the registry.
declare -A tested
for platform in "${platforms[@]}"; do
  file="${archive[$platform]}"
  if [ ! -f "$file" ]; then
    fail "the image for linux/$platform is missing: no archive at $file. Nothing was published"
  fi
  tested[$platform]="$("$scripts/image-id.sh" "$file")"
  config="$(tar --extract --to-stdout --file "$file" manifest.json | jq --raw-output '.[0].Config')"
  built_for="$(tar --extract --to-stdout --file "$file" "$config" | jq --raw-output '"\(.os)/\(.architecture)"')"
  if [ "$built_for" != "linux/$platform" ]; then
    fail "the archive given for linux/$platform holds an image for $built_for. Nothing was published"
  fi
  say "linux/$platform: the tested image is ${tested[$platform]}"
done

# --- 3. Login -----------------------------------------------------------------

image="$REGISTRY_HOST/butterbot"

trap 'docker logout "$REGISTRY_HOST" >&3 2>&3 || true' EXIT
if ! printf '%s' "$REGISTRY_PASSWORD" \
  | docker login "$REGISTRY_HOST" --username "$REGISTRY_USERNAME" --password-stdin >&3 2>&3; then
  fail "the registry refused the login. Nothing was published"
fi

# Prints the manifest that a reference names, as the registry stores it.
# Status 1 means that the registry has no such manifest. Anything else that
# goes wrong ends with status 2, so that a registry that cannot be read is
# never taken for an empty one.
raw_manifest() {
  local output errors
  errors="$(mktemp)"
  if output="$(docker buildx imagetools inspect --raw "$1" 2> "$errors")"; then
    rm -f "$errors"
    printf '%s' "$output"
    return 0
  fi
  cat "$errors" >&3
  if grep --quiet --ignore-case --extended-regexp 'not found|manifest unknown|name unknown' "$errors"; then
    rm -f "$errors"
    return 1
  fi
  rm -f "$errors"
  echo "publish-image: the registry could not be read; see the output of the tools" >&2
  exit 2
}

# The digest of a manifest as the registry stores it.
digest_of() {
  printf 'sha256:%s' "$(printf '%s' "$1" | sha256sum | cut -d ' ' -f 1)"
}

# --- 4. A version that exists is not overwritten ------------------------------

complete=""
found=0
existing="$(raw_manifest "$image:$version")" || found=$?
if [ "$found" -gt 1 ]; then
  exit 1
fi
if [ "$found" -eq 0 ]; then
  # The tag exists. It may stay only if it names exactly the images of this
  # release, which is the case when the publishing job is run again.
  same="yes"
  for platform in "${platforms[@]}"; do
    entry="$(printf '%s' "$existing" | jq --raw-output --arg arch "$platform" \
      '[.manifests[]? | select(.platform.os == "linux" and .platform.architecture == $arch) | .digest] | if length == 1 then .[0] else "" end')"
    if [ -z "$entry" ]; then
      same=""
      break
    fi
    published="$(raw_manifest "$image@$entry" | jq --raw-output '.config.digest')"
    if [ "$published" != "${tested[$platform]}" ]; then
      same=""
      break
    fi
  done
  listed="$(printf '%s' "$existing" | jq '[.manifests[]? | select(.platform.os != "unknown")] | length')"
  if [ -z "$same" ] || [ "$listed" != "${#platforms[@]}" ]; then
    fail "butterbot $version is already in the registry, from another build. Nothing was published. Release the next version, or delete the tags of $version in the registry first"
  fi
  say "butterbot $version is already in the registry with these images; completing the release"
  complete="yes"
fi

# --- 5. The image of each platform --------------------------------------------

sources=()
if [ -z "$complete" ]; then
  for platform in "${platforms[@]}"; do
    loaded="$(docker load --input "${archive[$platform]}" 2>&3 | tee /dev/fd/3 | sed -n 's/^Loaded image: //p' | head -n 1)"
    if [ -z "$loaded" ]; then
      fail "the archive for linux/$platform could not be loaded. No tag of $version was set"
    fi
    # Docker pushes by tag. The image of each platform gets a tag of its own.
    reference="$image:$version-linux-$platform"
    docker tag "$loaded" "$reference" >&3 2>&3
    if ! docker push "$reference" >&3 2>&3; then
      fail "the image for linux/$platform could not be pushed. No tag of $version was set"
    fi
    # What is in the registry now has to be the image that was tested.
    manifest="$(raw_manifest "$reference")" \
      || fail "the image for linux/$platform is not in the registry after the push. No tag of $version was set"
    published="$(printf '%s' "$manifest" | jq --raw-output '.config.digest')"
    if [ "$published" != "${tested[$platform]}" ]; then
      fail "the image pushed for linux/$platform is $published, not the tested ${tested[$platform]}. No tag of $version was set"
    fi
    sources+=("$image@$(digest_of "$manifest")")
    say "linux/$platform: pushed"
  done
fi

# --- 6. One image for both platforms, and the tags ----------------------------

if [ -z "$complete" ]; then
  targets=()
  for tag in "${tags[@]}"; do
    targets+=(--tag "$image:$tag")
  done
  docker buildx imagetools create "${targets[@]}" "${sources[@]}" >&3 2>&3 \
    || fail "the image for both platforms could not be created"
else
  # The image for both platforms is there. Point the other tags at it.
  for tag in "${tags[@]}"; do
    [ "$tag" = "$version" ] && continue
    docker buildx imagetools create --tag "$image:$tag" "$image:$version" >&3 2>&3 \
      || fail "the tag $tag could not be set"
  done
fi

released="$(raw_manifest "$image:$version")" \
  || fail "butterbot $version is not in the registry after the release"
digest="$(digest_of "$released")"

# --- 7. What was published -----------------------------------------------------

listing=""
for tag in "${tags[@]}"; do
  listing+="${listing:+, }\`$tag\`"
done
summary="### Released butterbot $version

| | |
|---|---|
| Image | \`butterbot\` |
| Version | \`$version\` |
| Tags | $listing |
| Digest | \`$digest\` |
| Platforms | \`linux/amd64\`, \`linux/arm64\` |"

say "released butterbot $version"
say "tags: ${tags[*]}"
say "digest: $digest"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  printf '%s\n' "$summary" >> "$GITHUB_STEP_SUMMARY"
fi
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "digest=$digest" >> "$GITHUB_OUTPUT"
fi
