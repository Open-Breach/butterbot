#!/usr/bin/env bash
# Proves that the rehearsal notices broken publishing steps: a copy of the
# scripts in which a pre-release also sets `X.Y` and `latest` must fail the
# rehearsal, and for that reason.
#
#   rehearse-broken-publish.sh <archive of linux/amd64> <archive of linux/arm64>
#
# A rehearsal that passes whatever the publishing steps do would prove
# nothing about them.
set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
amd64="${1:?usage: rehearse-broken-publish.sh <archive of linux/amd64> <archive of linux/arm64>}"
arm64="${2:?usage: rehearse-broken-publish.sh <archive of linux/amd64> <archive of linux/arm64>}"

broken="$(mktemp -d)"
trap 'rm -rf "$broken"' EXIT
cp "$scripts"/*.sh "$broken/"

# The rule for a pre-release, taken out.
# shellcheck disable=SC2016  # the text of the script, not an expansion
sed 's/^if \[ -z "${BASH_REMATCH\[4\]}" \]; then$/if true; then/' "$scripts/publish-image.sh" > "$broken/publish-image.sh"
if cmp -s "$scripts/publish-image.sh" "$broken/publish-image.sh"; then
  echo "the broken copy of publish-image.sh is not broken: the rule for a pre-release was not found" >&2
  exit 1
fi

if "$broken/rehearse-publish.sh" "$amd64" "$arm64" > "$broken/rehearsal.log" 2>&1; then
  cat "$broken/rehearsal.log"
  echo "The rehearsal passed although a pre-release moved latest." >&2
  exit 1
fi
if ! grep -a -q 'FAILED: latest still names 0.1.0' "$broken/rehearsal.log"; then
  cat "$broken/rehearsal.log"
  echo "The rehearsal failed, but not because a pre-release moved latest." >&2
  exit 1
fi
grep -a 'FAILED' "$broken/rehearsal.log"
echo "The rehearsal fails when the publishing steps are broken."
