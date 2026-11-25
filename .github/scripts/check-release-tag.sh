#!/usr/bin/env bash
# Checks the tag of a release before anything is built:
#
#   - the tag has the form v<semantic version>, such as v0.1.0 or
#     v0.2.0-rc.1;
#   - the version is the version of the program at the tagged commit, as
#     Cargo reports it;
#   - the tagged commit is contained in the branch main.
#
#   check-release-tag.sh <tag>
#
# It runs in a clone of the repository that is checked out at the tag and
# has the history of `origin/main`, so the checkout must not be shallow. The
# two checks of the version and of the commit are both made, and each one
# that fails has a message of its own. On success the version is written to
# the step's outputs (`version`).
set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=.github/scripts/version.sh
. "$scripts/version.sh"

tag="${1:?usage: check-release-tag.sh <tag>}"
main="origin/main"

refuse() {
  printf 'check-release-tag: %s\n' "$*" >&2
  exit 1
}

# The form. A pattern in a workflow admits every tag that begins with `v`;
# this is where `v1` and `vfoo` end.
version="${tag#v}"
if [ "$version" = "$tag" ] || ! [[ "$version" =~ $semantic_version ]]; then
  hint=""
  case "$tag" in
    *+*) hint=" Build metadata (+...) is not accepted, because an image tag cannot contain +." ;;
  esac
  refuse "the tag \"$tag\" is not a version tag. A release is started by a tag of the form v<semantic version>, such as v0.1.0 or v0.2.0-rc.1.$hint"
fi
case "$version" in
  *-linux-amd64 | *-linux-arm64)
    refuse "the tag \"$tag\" ends like the tag of one platform's image. A version must not end in -linux-amd64 or -linux-arm64."
    ;;
esac

commit="$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}")" \
  || refuse "this clone has no tag \"$tag\""
# The version is read from the files that are checked out.
if [ "$(git rev-parse --verify HEAD)" != "$commit" ]; then
  refuse "the tag \"$tag\" is on commit $commit, but another commit is checked out"
fi

failed=0

# The version of the program, from Cargo and not from reading Cargo.toml.
program="$(cargo metadata --no-deps --format-version 1 \
  | jq --raw-output '.packages[] | select(.name == "butterbot") | .version')"
if [ -z "$program" ]; then
  refuse "Cargo reports no package butterbot here"
fi
if [ "$program" != "$version" ]; then
  printf 'check-release-tag: the versions differ: the tag "%s" says %s, but the program at the tagged commit is version %s. Set the version in Cargo.toml, or tag the commit that has version %s.\n' \
    "$tag" "$version" "$program" "$version" >&2
  failed=1
fi

# The tagged commit must lie on main.
git rev-parse --verify --quiet "$main^{commit}" > /dev/null \
  || refuse "this clone has no branch $main to compare with. The checkout must fetch the whole history."
if ! git merge-base --is-ancestor "$commit" "$main"; then
  printf 'check-release-tag: the commit is outside main: the tag "%s" is on commit %s, which is not contained in the branch main. Merge into main first, then tag a commit of main.\n' \
    "$tag" "$commit" >&2
  failed=1
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi

printf 'version=%s\n' "$version" >> "${GITHUB_OUTPUT:-/dev/null}"
echo "The tag $tag is version $version of the program, and its commit $commit is contained in main."
