# Proposal

## Why

Butterbot runs as a container by default, but nothing puts its image into a registry: the image is built and tested on every change and then thrown away. The owner wants a released version to arrive in his own registry without building it by hand, and the repository is public, so it must not say which registry that is.

## What Changes

- A release workflow that starts when a version tag (`v0.1.0`) is pushed. It checks that the tag matches the program's version and lies on `main`, runs the checks that run on every change, and then publishes the image.
- The image goes to a private registry that is named only in the secrets of a deployment environment called `release`. No file, log line, run summary or attestation names the registry.
- One image name for `linux/amd64` and `linux/arm64`, with the tags `0.1.0`, `0.1` and `latest`. A pre-release version gets only its own tag.
- The image that is published for a platform is the image the container tests ran against, not a second build.
- Exactly one job reads the registry secrets, and it runs last. With a required reviewer on the environment, a release waits for one approval, given with the results of the checks in view.
- A version that is already in the registry is not overwritten. Running the failed job of a release again completes that release.
- A build provenance attestation for the published image, kept with GitHub.
- A rehearsal on every change: the publishing steps run against a throwaway registry, so the first real release is not the first time they run.
- The documents say plainly what the project offers: a console application built from source, and no published image. They keep the commands for building an image from the `Dockerfile`, and gain a page on releasing.

Not part of this change:

- A public image. The owner has decided (2026-11-24) that the project offers the console application as such for now. If a public image is offered later, it will be published through Trebuchet, the distribution repository of Open-Breach, not through a second path in this repository.
- Prebuilt downloads of the program.
- Deploying the released image to a cluster. Deployment configuration lives outside this repository.
- Removing old images from the registry.

## Capabilities

### New Capabilities

- `image-release`: how a released version becomes a container image in a private registry, including what starts a release, what is checked first, tags, platforms, secrecy of the registry, approval, attestation and the rehearsal.

### Modified Capabilities

None. The `container-image` capability describes the image itself and is unchanged.

## Impact

- **Workflows**: a new release workflow; the existing CI workflow becomes callable from it, keeps each tested image for the publishing step, and gains the rehearsal.
- **Scripts**: the publishing steps and the checks of a tag live in scripts that the release and the rehearsal share.
- **Documents**: `README.md`, `docs/deployment.md`, and a new `docs/releasing.md`.
- **Program code**: none. `butterbot` itself does not change.
- **Outside the repository, done by the owner**: the `release` environment with a required reviewer, a rule that admits only version tags, and three secrets; a login at the registry for the pipeline; whatever the cluster needs to pull from that registry.
