# Releasing

A released version of Butterbot becomes a container image in a private registry. The project itself publishes no image; this page is for whoever maintains a copy of the repository and has a registry of their own.

The repository does not say which registry that is. The registry's host and the login are secrets of a deployment environment, and no file, log, summary or attestation names either. Every example on this page uses `registry.example.com`.

- [Cutting a release](#cutting-a-release)
- [What the workflow does](#what-the-workflow-does)
- [Set-up outside the repository](#set-up-outside-the-repository)
- [After a release](#after-a-release)
- [When a release fails](#when-a-release-fails)
- [What the publishing steps do](#what-the-publishing-steps-do)
- [How the images of the two platforms reach the registry](#how-the-images-of-the-two-platforms-reach-the-registry)
- [What the registry holds afterwards](#what-the-registry-holds-afterwards)
- [The rehearsal](#the-rehearsal)

## Cutting a release

1. **Set the version.** The version of the program is `version` in `Cargo.toml`. Change it, run `cargo check` so that `Cargo.lock` follows, and bring both files to `main` the usual way. A pre-release has a suffix: `0.2.0-rc.1`.

2. **Tag a commit of `main` and push the tag.** The tag is the version with a `v` in front.

   ```
   git switch main
   git pull
   git tag v0.1.0
   git push origin v0.1.0
   ```

   Nothing else starts a release: no push to a branch, no pull request, no tag of another form.

3. **Wait for the checks.** The run is under "Actions", in the workflow "Release". From a terminal:

   ```
   gh run list --workflow release.yml
   gh run watch <run id>
   ```

4. **Approve.** When the checks have passed, the run waits at the job `publish`. Open the run, choose "Review deployments", tick `release` and approve. Nothing is pushed to the registry before that.

5. **Check the result.** See [After a release](#after-a-release).

## What the workflow does

`.github/workflows/release.yml` has three jobs, in this order. A job runs only when the ones before it have passed.

| Job | What it does |
|---|---|
| `verify` | Checks the tag: it has the form `v<semantic version>`, the version is the version of the program at the tagged commit (as Cargo reports it), and the tagged commit is contained in `main`. |
| `checks` | Runs the CI workflow on the tagged commit: everything that runs for a change, including the container tests on both platforms and the rehearsal. Each `image` job hands on the image its container tests ran against. |
| `publish` | Publishes those two images as one multi-platform image, and records an attestation for it. It builds nothing. |

What `verify` refuses, each with a message of its own:

| Tag | Message |
|---|---|
| `v1`, `vfoo`, `v0.1.0+build.5` | The tag is not a version tag. Build metadata is not accepted, because an image tag cannot contain `+`. |
| `v0.2.0` on a commit whose `Cargo.toml` says `0.1.0` | The versions differ, with both versions. |
| a version tag on a commit that `main` does not contain | The commit is outside `main`. |

`publish` is the only job that refers to the deployment environment `release`, so it is the only job that can read the registry's host and login, and the only one that waits for an approval. It is also the only job that may request a signing certificate and store an attestation. Every other job of every workflow can only read the repository.

Only one release runs at a time. A second one waits until the first has finished. GitHub keeps one waiting run, so a third would take the place of the second.

## Set-up outside the repository

Nothing of this is in the repository, and the workflow cannot create or check it.

**The environment.** Under "Settings", "Environments", an environment named `release` with:

- **Required reviewers**: whoever approves a release.
- **Prevent self-review**: off, if the person who pushes the tag is also the one who approves.
- **Deployment branches and tags**: "Selected branches and tags", with one rule for tags, `v*`. Without it, a workflow on any branch that names the environment would be put before the reviewer for approval.
- **Environment secrets**, three of them:

  | Secret | Example | What it is |
  |---|---|---|
  | `REGISTRY_HOST` | `registry.example.com` | The host of the registry, as Docker takes it: no `https://`, no path, no slash at the end. With a port if the registry has one: `registry.example.com:5000`. |
  | `REGISTRY_USERNAME` | `butterbot-release` | The user of the login. |
  | `REGISTRY_PASSWORD` | | Its password or token. |

  They are secrets of the environment, not of the repository. A repository secret can be read by any workflow on any branch.

The image is published as `<REGISTRY_HOST>/butterbot`. Another path is not supported.

If a release runs before the environment exists, GitHub creates an empty environment without protection. The publishing job then stops at its first check, which names the secrets that are missing.

**A login for the pipeline.** Give the pipeline a login of its own at the registry, separate from any personal one, so that it can be withdrawn on its own. If the registry can restrict a login to one image, restrict it to `butterbot`, with the right to push.

GitHub masks the three values wherever they appear in a log. It masks exact values only, so a user name that is a common word would be masked in every place that word appears; pick one that is not.

**The cluster.** Whatever it needs to pull from the registry. That is outside this repository; see [Deployment](deployment.md).

## After a release

The summary of the run shows the version, the tags that were set and the digest of the multi-platform image. It names the image as `butterbot`, without the registry.

**The log.** Open the job `publish` and read the step "Publish the image". The registry's host must appear as `***` wherever a tool names it.

**The registry.** Log in to the registry and compare what it holds with the summary:

```
docker login registry.example.com
docker buildx imagetools inspect registry.example.com/butterbot:0.1.0
```

`Digest` is the digest from the summary, and the manifests are for `linux/amd64` and `linux/arm64`. The tags `0.1` and `latest` show the same digest.

**The attestation.** The release records a build provenance attestation for that digest. It is kept with GitHub, not in the registry, and it names its subject as `butterbot`: attestations of a public repository are written to a public log, so the registry's host is not part of it. To verify the image in the registry against the repository, with the same login:

```
gh attestation verify oci://registry.example.com/butterbot:0.1.0 --repo Open-Breach/butterbot
```

The command reads the digest of the image from the registry and looks the attestation up at GitHub by that digest. It succeeds when GitHub holds an attestation for that digest that a workflow of this repository signed. To require the release workflow and the tag as well:

```
gh attestation verify oci://registry.example.com/butterbot:0.1.0 --repo Open-Breach/butterbot \
  --signer-workflow Open-Breach/butterbot/.github/workflows/release.yml \
  --source-ref refs/tags/v0.1.0
```

Do not add `--deny-self-hosted-runners`: for Open-Breach the release runs on Depot's runners, which GitHub records as self-hosted, and the verification would fail for that reason alone.

To see the attestation without the registry, by the digest from the summary:

```
gh api repos/Open-Breach/butterbot/attestations/sha256:<digest>
```

**The image.** Pull it on a machine of the platform it will run on, and run it:

```
docker pull registry.example.com/butterbot:0.1.0
docker run --rm registry.example.com/butterbot:0.1.0 --version
```

Deployments should name a version or a digest (`registry.example.com/butterbot@sha256:<digest>`), not `latest`.

## When a release fails

Nothing is published before the job `publish`, so a release that fails in `verify` or `checks` leaves the registry as it was.

| Where | What to do |
|---|---|
| `verify` | The tag is wrong. Delete it, here and at GitHub, correct what the message names, and tag again: `git tag --delete v0.1.0`, then `git push --delete origin v0.1.0`. |
| `checks` | A check failed on the tagged commit. Delete the tag, fix the fault on `main`, and tag the new commit. |
| `publish`, a secret is missing | Add it to the environment and run the failed job again. |
| `publish`, the login is refused | Correct the secrets, or the login at the registry, and run the failed job again. |
| `publish`, after something was pushed | Run the failed job again. It publishes the same two images, finds what is already there and completes the rest. This also holds when the tags are set and the attestation failed. |
| `publish`, the version exists with other images | The registry holds this version from another build. A released version is not overwritten. Release the next version, or delete the tags of this version in the registry (`0.1.0`, `0.1.0-linux-amd64`, `0.1.0-linux-arm64`) and run the failed job again. |

To run the failed job again, choose "Re-run failed jobs" on the run, or:

```
gh run rerun <run id> --failed
```

The job asks for the approval again. It uses the images of the same run, which is why it can complete a release: they are the same images, with the same identifiers. They are kept for seven days. After that, or when "Re-run all jobs" is chosen, or when the tag is pushed again, the images are built anew and are other images; if the version's tag is already in the registry, the release is refused as the last row describes.

**Withdrawing a release.** Delete its tags in the registry. If `latest` or `X.Y` named it, they are gone too until the next release sets them; deployments that name another version or a digest are not affected.

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

What it cannot check: HTTPS to a real registry, the real login, the approval of the environment and the attestation. They are exercised for the first time by the first release. The rehearsal's registry speaks plain HTTP on `localhost`, which Docker and `buildx` accept for that name without being told.

To run it on a Linux machine with Docker, `curl`, `jq` and `htpasswd`:

```
docker build --provenance=false --platform linux/amd64 --tag butterbot:dev . && docker save --output amd64.tar butterbot:dev
docker build --provenance=false --platform linux/arm64 --tag butterbot:dev . && docker save --output arm64.tar butterbot:dev
.github/scripts/rehearse-publish.sh amd64.tar arm64.tar
```

`--provenance=false` keeps the image one image. Without it, Docker's containerd image store attaches an attestation to the build, and what is saved and pushed is a list of manifests; the publishing steps refuse that. Building the platform that is not the machine's own needs emulation and is slow; the CI workflow builds each on a machine of its platform.
