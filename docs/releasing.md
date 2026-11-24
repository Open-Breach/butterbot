# Releasing

A released version of Butterbot becomes a container image in a private registry. The project itself publishes no image; this page is for whoever maintains a copy of the repository and has a registry of their own.

The repository does not say which registry that is. The registry's host and the login are given to the publishing steps from outside, and nothing they write themselves names either. Every example on this page uses `registry.example.com`.

- [What the publishing steps do](#what-the-publishing-steps-do)
- [How the images of the two platforms reach the registry](#how-the-images-of-the-two-platforms-reach-the-registry)
- [What the registry holds afterwards](#what-the-registry-holds-afterwards)
- [The rehearsal](#the-rehearsal)

## What the publishing steps do

The steps are one script, `.github/scripts/publish-image.sh`. It publishes two images that already exist, one for `linux/amd64` and one for `linux/arm64`, as one image `butterbot` that serves both platforms. It builds nothing.

```
REGISTRY_HOST=registry.example.com REGISTRY_USERNAME=... REGISTRY_PASSWORD=... \
  .github/scripts/publish-image.sh 0.1.0 image-linux-amd64.tar image-linux-arm64.tar
```

The two archives are the ones the `image` jobs of the CI workflow write with `docker save`, after the container tests have run against the image. The image is published as `<REGISTRY_HOST>/butterbot`, here `registry.example.com/butterbot`.

In order:

1. **Check the settings.** If `REGISTRY_HOST`, `REGISTRY_USERNAME` or `REGISTRY_PASSWORD` is missing or empty, the script names it and stops, before it contacts anything. A version that is not of the form `X.Y.Z` or `X.Y.Z-<suffix>` stops it as well.
2. **Log in** to the registry.
3. **Read the two images.** The script reads each archive's image identifier and platform from the archive, and loads the archive into Docker. If an archive is missing, or holds an image for the wrong platform, it stops. Nothing has been pushed at this point.
4. **Check whether the version exists.** If the registry already holds the tag of the full version, the script compares the images that tag names with the two it was given. Other images: it stops, and the registry is unchanged. The same images: it goes on, so that a publishing job that failed half way can be run again.
5. **Push the image of each platform**, and check that the registry then holds the image with the identifier from the archive.
6. **Create the multi-platform image and set the tags.** This is the last change to the registry, and the only one that sets or moves a tag of the release.
7. **Report.** The version, the tags and the digest of the multi-platform image go to the summary of the run. The digest is also an output of the step (`digest`), for the steps after it.

The **image identifier** is the digest of the image's configuration. It is the same in the archive, in the registry and on a machine that has pulled the image, whatever the tools in between do to the layers. It is how the script knows that the image in the registry is the image that was tested.

The tags of a release:

| Version | Tags that are set |
|---|---|
| `0.1.0` | `0.1.0`, `0.1` and `latest` |
| `0.2.0-rc.1`, a pre-release | `0.2.0-rc.1` only. `0.2`, `0.1` and `latest` stay where they are. |

`latest` names the release that was published last, not the highest version. Name a version or a digest where it matters.

When something goes wrong:

| What happens | Result |
|---|---|
| A setting is missing | Exit code 2. The message names the setting. No registry was contacted. |
| The login is refused | Exit code 1. Nothing was pushed. |
| An archive is missing, or is for the wrong platform | Exit code 1. Nothing was pushed. |
| The version is in the registry with other images | Exit code 1. Nothing was pushed. Delete the tags of that version in the registry, or release the next version. |
| The image of the second platform cannot be pushed | Exit code 1. The first platform's image is in the registry under its own tag. No tag of the release was set or moved. |
| The tags cannot be set, or only some of them | Exit code 1. Run the script again with the same archives: it completes what is missing. |

The script never prints the host, the user or the password, and it does not run with command tracing. What the tools it calls print is another matter: `docker push` names the repository it pushes to. In a workflow the three values are secrets, and GitHub masks a secret wherever its exact value appears in the log. The script hands the host to the tools as it is, without changing it, so that the masking applies.

## How the images of the two platforms reach the registry

Under a tag per platform. For version `0.1.0` the script runs, in this order:

```
docker load --input image-linux-amd64.tar
docker tag <the loaded image> registry.example.com/butterbot:0.1.0-linux-amd64
docker push registry.example.com/butterbot:0.1.0-linux-amd64
```

the same for `linux/arm64` with `0.1.0-linux-arm64`, and then:

```
docker buildx imagetools create \
  --tag registry.example.com/butterbot:0.1.0 \
  --tag registry.example.com/butterbot:0.1 \
  --tag registry.example.com/butterbot:latest \
  registry.example.com/butterbot@sha256:<manifest of the amd64 image> \
  registry.example.com/butterbot@sha256:<manifest of the arm64 image>
```

`imagetools create` writes one manifest that lists the two images and puts it under the three tags. It refers to the two images by digest, not by their tags.

Why a tag per platform and not a push by digest: `docker push` cannot push an image that has no tag, and pushing by digest would need another tool in the job that holds the registry login. Docker and its `buildx` plugin are on every runner. The script uses only what every registry offers: pushing an image, pushing a manifest that lists images, and reading manifests.

The per-platform tags are not tags of the release, and nothing refers to them afterwards. Whether they can be deleted once the release is complete depends on the registry: some remove a manifest that no tag names, even when a multi-platform manifest still lists it. If in doubt, leave them.

A version that ends in `-linux-amd64` or `-linux-arm64` is refused, because its tag would be the per-platform tag of another version.

## What the registry holds afterwards

After `0.1.0` and then `0.2.0-rc.1`:

| Tag | Names |
|---|---|
| `0.1.0`, `0.1`, `latest` | the multi-platform image of `0.1.0` |
| `0.1.0-linux-amd64`, `0.1.0-linux-arm64` | the image of one platform of `0.1.0` |
| `0.2.0-rc.1` | the multi-platform image of `0.2.0-rc.1` |
| `0.2.0-rc.1-linux-amd64`, `0.2.0-rc.1-linux-arm64` | the image of one platform of `0.2.0-rc.1` |

A multi-platform image lists exactly `linux/amd64` and `linux/arm64`. Pulling a tag yields the image for the platform of the machine that pulls.

## The rehearsal

The publishing steps run on every change, in the job `rehearsal` of the CI workflow, so that a release is not the first time they run. The job starts a registry on its own machine that requires a login, makes up the login, and runs `publish-image.sh` against it with the two images that the same run has built and tested. It uses no secret. The script is `.github/scripts/rehearse-publish.sh`; it reads what the registry holds over HTTP, not with the tools the publishing steps use.

What it checks:

- The registry refuses a request without a login.
- A release of `0.1.0` sets `0.1.0`, `0.1` and `latest` to one image that lists exactly the two platforms. For each platform the tag yields the image with the identifier of the tested one, and `docker pull` for that platform receives it.
- The summary has the version, the tags and one digest, and names the image as `butterbot`. The digest is handed on as an output.
- Nothing the script writes itself contains the registry's host, the user or the password.
- A second run with the same images completes and leaves every tag where it was.
- Another build of `0.1.0` is refused, and the registry is unchanged.
- `0.2.0-rc.1` after `0.1.0` sets only its own tag; `latest` and `0.1` still name `0.1.0`.
- With one archive missing, nothing reaches the registry. With a push of the second platform that fails, no tag of the release is set or moved.
- A login the registry does not know stops the script, and nothing is pushed.

What it cannot check: HTTPS to a real registry and the real login. The rehearsal's registry speaks plain HTTP on `localhost`, which Docker and `buildx` accept for that name without being told.

To run it on a Linux machine with Docker, `curl`, `jq` and `htpasswd`:

```
docker build --provenance=false --platform linux/amd64 --tag butterbot:dev . && docker save --output amd64.tar butterbot:dev
docker build --provenance=false --platform linux/arm64 --tag butterbot:dev . && docker save --output arm64.tar butterbot:dev
.github/scripts/rehearse-publish.sh amd64.tar arm64.tar
```

`--provenance=false` keeps the image one image. Without it, Docker's containerd image store attaches an attestation to the build, and what is saved and pushed is a list of manifests; the publishing steps refuse that. Building the platform that is not the machine's own needs emulation and is slow; the CI workflow builds each on a machine of its platform.
