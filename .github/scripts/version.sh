# shellcheck shell=bash
# Sourced by the scripts that read a version.
#
# A semantic version as a release accepts it: X.Y.Z, or X.Y.Z-<suffix> for a
# pre-release. Build metadata (`+...`) is not accepted, because an image tag
# cannot contain `+`. After a match, BASH_REMATCH holds X, Y and Z at 1 to 3,
# and the suffix with its hyphen at 4.
# shellcheck disable=SC2034  # used by the scripts that source this file
semantic_version='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(\.(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?$'
