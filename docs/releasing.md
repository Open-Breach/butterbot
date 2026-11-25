# Releasing

A release turns a version of Butterbot into a container image in a private registry. This page describes what is published and how. The registry is not named anywhere in this repository; the examples use `registry.example.com`.

- [What a release publishes](#what-a-release-publishes)
- [The publishing steps](#the-publishing-steps)
- [The rehearsal](#the-rehearsal)

## What a release publishes

One image, `<registry>/butterbot`, for `linux/amd64` and `linux/arm64`. Pulling a tag yields the image for the platform of the machine that pulls.

| Version | Tags that are set |
|---|---|
| `0.1.0` | `0.1.0`, `0.1`, `latest` |
| `0.2.0-rc.1`, a pre-release | `0.2.0-rc.1` only |

A pre-release moves neither `latest` nor the tag of its line.

The image published for a platform is the image the container tests ran against on a machine of that platform. Nothing is built a second time: the image jobs of the checks save their tested image, and the release publishes those archives.

A deployment should name a version or a digest, not `latest`.

## The publishing steps

The steps are one script, `.github/scripts/publish-image.sh`. It is given the version and the two image archives, and reads the registry from three settings: `REGISTRY_HOST`, `REGISTRY_USERNAME` and `REGISTRY_PASSWORD`. In order:

1. **Settings.** If one of the three is missing or empty, the script stops and names it. It prints no value and contacts no registry.
2. **Archives.** Both archives must be there, and each must hold an image for its platform. The identifier of each image is read from its archive: the digest of the image's configuration, which is the same in the archive and in a registry.
3. **Login.**
4. **A version that exists.** If the registry already holds the tag of the version, the script compares the images that tag names with the ones it was given.
   - Other images: it stops. Nothing is pushed and no tag moves. A released version is not overwritten.
   - The same images: it goes on, sets the tags that are missing and changes nothing else. This is what happens when the publishing job of a release is run again.
5. **The image of each platform.** Docker pushes by tag, so each platform's image is pushed under a tag of its own: `0.1.0-linux-amd64` and `0.1.0-linux-arm64`. After each push the script reads the image back from the registry and stops if its identifier is not the tested one.
6. **One image for both platforms, and the tags.** The image for both platforms is created from the two that were pushed, and the tags of the release are set on it. This is the last change to the registry, so a release that fails earlier moves none of its tags.
7. **Report.** The version, the tags and the digest of the image for both platforms go to the summary of the run, which names the image as `butterbot`.

The tags `0.1.0-linux-amd64` and `0.1.0-linux-arm64` stay in the registry. They name the single-platform images that `0.1.0` consists of. Nothing reads these two tags afterwards, but the images they name must stay: deleting them would break `0.1.0`.

The script uses only what every registry with the OCI interface offers: pushing an image, pushing an image index, and reading a manifest.

**The registry stays out of the log.** The script never writes the registry's host or the login itself; its own messages name the image as `butterbot`. Docker and its tools do print the host. In a release the host is a secret of the workflow, and GitHub replaces it in the log wherever a tool prints it.

## The rehearsal

Every change rehearses a release. The job `release rehearsal` of the checks starts a registry on its runner that requires a login made up in the job, and runs the same script against it with the images that the change's own image jobs have tested. `.github/scripts/rehearse-release.sh` then reads the registry and checks:

- a missing setting, a missing archive, archives the wrong way round and a refused login each stop the script with nothing published;
- a release sets `0.1.0`, `0.1` and `latest` on one image that lists exactly `linux/amd64` and `linux/arm64`, and each platform's image has the identifier of the tested one;
- the summary has the version, the tags and the digest, and nothing the script writes itself contains the registry's host, the login or the password;
- running the release again with the same images completes it and leaves the registry as it was, also when only the tag of the version had been set;
- another build of a released version is refused and the registry is unchanged;
- a pre-release sets only its own tag;
- a release that lacks a platform moves no tag.

The rehearsal uses no secret and attests nothing. It cannot cover HTTPS to the real registry, its login, or the approval of a release.

It can be run on any machine with Docker, given two image archives:

```
docker build --provenance=false --tag butterbot:dev .
docker save --output amd64.tar butterbot:dev
.github/scripts/rehearse-release.sh amd64.tar arm64.tar
```

The second archive has to come from a machine of the other platform.
