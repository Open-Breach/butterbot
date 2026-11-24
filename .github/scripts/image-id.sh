#!/usr/bin/env bash
# Prints the identifier of the image in an archive written by `docker save`:
# the digest of the image's configuration, as `sha256:<hex>`. It is what a
# registry's manifest of the image names as its `config`, and what Docker's
# classic image store shows as the image's ID. (The containerd image store
# shows the digest of the manifest instead.)
#
#   image-id.sh <archive>
#
# The archive must hold exactly one image. The identifier is read from the
# archive alone, without Docker, and the configuration is hashed again, so
# that an archive that names one image and holds another is refused.
set -euo pipefail

archive="${1:?usage: image-id.sh <archive>}"

if [ ! -f "$archive" ]; then
  echo "image-id: no image archive at $archive" >&2
  exit 1
fi

# `manifest.json` lists the images of the archive; `Config` is the path of an
# image's configuration inside the archive, named after its digest.
config="$(tar --extract --to-stdout --file "$archive" manifest.json \
  | jq --raw-output 'if length == 1 then .[0].Config else error("the archive holds \(length) images, not one") end')"

named="$(basename "$config" .json)"
actual="$(tar --extract --to-stdout --file "$archive" "$config" | sha256sum | cut -d ' ' -f 1)"
if [ "$named" != "$actual" ]; then
  echo "image-id: the configuration in $archive is named $named, but its digest is $actual" >&2
  exit 1
fi

echo "sha256:$actual"
