# Design

## Context

See `proposal.md` for the motivation. What exists today:

- `Dockerfile` builds one statically linked program into an otherwise empty image, for `linux/amd64` and `linux/arm64`, each compiled on its own platform.
- `.github/workflows/ci.yml` runs on every push and pull request. Its `image` job runs once per platform on a machine of that platform: it builds `butterbot:dev`, runs `--version`, builds the `test` stage and runs the container tests. The images are discarded when the job ends.
- The jobs named GitHub's own runners (`ubuntu-latest` and `ubuntu-24.04-arm`) when this change was written. The builds of Open-Breach run on Depot's runners, and this change moves every job there; see D11.
- `tests/packaging.rs` holds checks that read repository files (the `Dockerfile`, `docs/deployment.md`, the README's links) without Docker.
- `docs/deployment.md` says that no image is published yet and shows how to build and push one by hand.
- The program version is `0.1.0` in `Cargo.toml`. No tag exists.

The owner's decisions that shape this change (2026-11-24):

- The image goes to the owner's own registry, not to a public one.
- The project itself offers the console application as such. A note says that it packs easily into a container.
- A public image may come later. It would then be published through Trebuchet, the distribution repository of Open-Breach.
- Releases are cut from version tags only. No image per merge to `dev`.

Constraints:

- The repository is public and so are its workflow logs. The registry's host is a real identifier and must not appear in either.
- The design assumes nothing about the registry beyond the OCI interface over HTTPS and a login with a user name and a password. It also assumes the worst about that login: that it is not limited to one image, so whoever holds it could overwrite other images in the same registry.
- Agents work in this repository with write access. They open and merge pull requests into `dev` and can push branches and tags.

## Goals / Non-Goals

**Goals:**

- Pushing a version tag is all it takes to release, apart from one approval.
- The registry login is exposed to as little as possible: one job, after the checks, behind the owner's approval.
- The image that reaches the registry is the image that was tested.
- The publishing steps are exercised on every change, so that a release does not fail on its first real use for a reason a test could have found.

**Non-Goals:**

- A public image, prebuilt downloads, or a GitHub Release page. See the proposal.
- Deploying to a cluster, or changing what runs there.
- Removing old images or per-platform tags from the registry.
- A registry path other than `<host>/butterbot`.
- Making `latest` follow the highest version rather than the most recent release. Releases of an older line after a newer one are not expected.

## Decisions

### D1. Three secrets on a deployment environment

The registry is described by `REGISTRY_HOST`, `REGISTRY_USERNAME` and `REGISTRY_PASSWORD`, stored as secrets of an environment named `release`.

- *Why secrets, also for the host:* GitHub masks secret values in logs. A repository variable would be printed by every tool that names the image.
- *Why an environment rather than repository secrets:* a repository secret can be read by any workflow on any branch, including one an agent adds. An environment secret is released only to a job that refers to the environment, after the environment's protection rules have passed.
- *Alternative considered:* one secret holding the full image name. Rejected, because tools print the host on its own (at login), and only the exact secret value is masked.

### D2. One publishing job, images handed over as artifacts

Each platform's `image` job saves the image it has tested (`docker save`) and uploads it as an artifact, `image-linux-amd64` and `image-linux-arm64`. A single `publish` job downloads both, loads them and pushes them.

- This gives one job that reads the secrets, hence one approval, and it makes the published image the tested one.
- The image is one static program, so the artifacts are small. Give them a short retention: seven days, which is long enough for a release that waits for its approval, and for running its publishing job again.
- *As built:* "the image that was tested" is established by the image identifier, which is the digest of the image's configuration. It is the same in an archive, in a registry and after a pull. Docker's own image ID is not a substitute: Depot's runners use Docker's containerd image store, which shows the digest of the manifest instead. For the same reason the image is built with `--provenance=false`: that store keeps the build's attestation beside the image, and what is saved and pushed is then a list of manifests and not one image.
- *Alternatives considered:* pushing from each platform job (the secrets in two jobs, the environment approved more than once, and a release half pushed when one platform fails); building both platforms again in the publishing job (an untested image, and emulation for one platform).

### D3. The release calls the CI workflow

`ci.yml` gains `workflow_call`, and the release workflow calls it instead of repeating its jobs. The artifacts uploaded by the called jobs are available to the release's later jobs.

- `ci.yml` currently runs on every push, which includes tags. Restrict its `push` trigger to branches, or a version tag would run the checks twice.
- *Alternative considered:* copying the jobs into the release workflow. Rejected: the two copies would drift.

### D4. Jobs of the release workflow

```
on: push of a tag v*
  verify   tag form, tag = program version, commit contained in main
  checks   the CI workflow (needs: verify)
  publish  environment: release (needs: verify, checks)
```

- `verify` reads the program version through Cargo (`cargo metadata` or `cargo pkgid`), not by parsing `Cargo.toml` by hand, and needs the history of `main` (`git merge-base --is-ancestor`), so its checkout must not be shallow.
- `publish` is the only job with `environment: release`, and the only one with the permissions an attestation needs (`id-token: write`, `attestations: write`). Everything else keeps `contents: read`.
- The workflow has a concurrency group, so that two releases never publish at the same time and `latest` cannot end up on the older one.
- A tag pattern in the workflow cannot express a semantic version exactly. The pattern admits `v*`; `verify` rejects anything that is not `v<semantic version>`.
- *As built:* `verify` also refuses a version with build metadata (`v0.1.0+build.5`), because an image tag cannot contain `+`, and it reports a differing version and a commit outside `main` each with a message of its own, both when both apply. The spec says so since this change was implemented.

### D5. The publishing steps are one script, shared with the rehearsal

The steps live in a script under `.github/scripts/`, for example `publish-image.sh`. It takes the registry host, the login, the version and the two image archives from its environment and arguments. The release runs it against the real registry; the rehearsal runs the same script against a throwaway one. The checks of `verify` live in a second script for the same reason.

The script, in order:

1. Fail if a setting is missing, naming it. Print no values.
2. Log in.
3. Load both archives and read each image's identifier.
4. If the registry holds the tag of the full version: compare the images it names with the loaded ones. Different: fail, having pushed nothing. Same: continue.
5. Push the image of each platform.
6. Create the multi-platform image from the two and set the tags: `X.Y.Z`, `X.Y` and `latest`, or only the full version for a pre-release.
7. Write the version, the tags and the digest of the multi-platform image to the run summary, and hand the digest to the attestation step.

Notes for the implementation:

- The script never echoes the host or the login, never runs with command tracing (`set -x`), and never transforms the host (no encoding, no splitting), because only the exact secret value is masked.
- The summary and the attestation name the image as `butterbot`.
- How the two platform images get into the registry before the tags are set is the implementer's choice. Pushing them under per-platform tags such as `0.1.0-linux-amd64` is acceptable; pushing by digest without a tag is tidier if the tools at hand do it reliably. Record the choice in `docs/releasing.md`.
- *As built:* per-platform tags, `X.Y.Z-linux-amd64` and `X.Y.Z-linux-arm64`, pushed with `docker push`; the multi-platform image is then created with `docker buildx imagetools create` from the two manifests by digest. `docker push` cannot push without a tag, and a push by digest would need another tool in the job that holds the login. The spec gained a requirement for these tags, and a version that ends like one is refused.
- *As built, found by rehearsing a second release:* a registry may refuse to move a tag from a manifest with Docker's media types to one with the OCI ones, or back. The registry of the rehearsal answers "400 Bad Request" to the tool's question for the tag, after the tag of the full version is set. Which media types a build writes depends on the builder's version and on Docker's image store. So the image jobs build with `--output type=docker,oci-mediatypes=false`, and the script compares the kind of what `X.Y` and `latest` hold with the kind of the release before it sets any tag; if they differ it stops and names the tag to delete.
- *As built:* when the registry cannot be asked whether the version exists, the script stops. Only the answer "not found" counts as "not there".
- Use only what every OCI registry offers: pushing images, pushing an image index, reading manifests. Do not rely on a referrers API or on registry-specific features.
- Check the current documentation of the tools used (Docker's `buildx imagetools`, or whichever tool is chosen) before relying on a flag. Versions and flags named here are a snapshot.

### D6. A released version is immutable, and a failed job can be run again

Step 4 above makes the tag of the full version immutable without making a failed release a dead end. "Re-run failed jobs" on a release uses the artifacts of that run, so the images are the same and the script completes what is missing. A new run from the same tag builds new images, whose identifiers differ, and is refused; `docs/releasing.md` says that the way out is to delete the version's tags in the registry, or to release the next version.

*Alternative considered:* refusing whenever the version exists. Rejected: an attestation step that fails after the tags are set would leave a release that cannot be finished without deleting tags by hand.

### D7. Attestation with GitHub, not in the registry

After the script, the publishing job records a build provenance attestation for the digest of the multi-platform image, with the subject name `butterbot`, using GitHub's attestation action. It is not pushed to the registry.

- Attestations of a public repository are signed through the public Sigstore instance and written to a public transparency log. The fully qualified image name would put the registry's host into that log for good, so the subject name is the bare image name.
- The owner verifies with the GitHub command line tool against the image in the registry (`gh attestation verify oci://<image> --repo <owner>/butterbot`); the tool finds the attestation by the image's digest.
- Check the action's current documentation for the input names and for which action is current (`actions/attest`, or `actions/attest-build-provenance`, which now wraps it).
- *As built (documentation read on 2026-11-24):* `actions/attest` v4.2.2, which `actions/attest-build-provenance` now wraps, with `subject-name: butterbot`, `subject-digest` from the publishing step and `push-to-registry: false`. With these inputs the statement names the subject as `butterbot` with its digest; the predicate describes the workflow run. The job asks for `id-token: write` and `attestations: write`. It does not ask for `artifact-metadata: write`, which the action needs only for a storage record, and it writes one only when the attestation is pushed to a registry. The host appears only on the owner's own command line when he verifies.
- **If it turns out that the attestation cannot be made or verified without the registry's host in it, leave the attestation step out, leave its tasks open, and report.** Do not put the host into an attestation.

### D8. The rehearsal

A job in `ci.yml`, after the two `image` jobs:

- starts a registry container on the machine, with a login required (a user and password generated in the job);
- runs the publishing script against it with the two image artifacts of that run and a made-up version;
- verifies the scenarios of the spec that do not need the real registry: the tags of a release and of a pre-release, both platforms under one name, the tested image being the published one, the refusal of a different build of an existing version, the completion of a repeated run, a missing setting, and a missing platform.

It uses no repository secret, so it also runs for pull requests from forks. It records no attestation. The throwaway registry speaks plain HTTP on `localhost`; how the chosen tools are told to accept that is part of the implementation. What the rehearsal cannot cover is HTTPS to the real registry, the environment's approval and the attestation; those are checked by the owner at the first release (see the tasks).

### D9. What the owner sets up outside the repository

The workflow cannot create or check these; `docs/releasing.md` describes them with example values only:

- The environment `release` with: the owner as required reviewer; "prevent self-review" off, since the owner both pushes the tag and approves; a deployment rule that admits only tags matching `v*`; the three secrets.
- A login at the registry for the pipeline, separate from any personal one, so that it can be withdrawn on its own.
- Whatever the cluster needs to pull from the registry.

If the workflow runs before the environment exists, GitHub creates an empty environment without protection. The missing-setting check (step 1 of the script) then stops the release, which is why that check comes first.

### D10. Documents

- `README.md`: the program is built from source; the project publishes no image; a short note that it packs into a container, pointing to `docs/deployment.md`. Add `docs/releasing.md` to the list of documents (`tests/packaging.rs` checks that the README links every document).
- `docs/deployment.md`: "No image is published yet" becomes a plain statement that the project publishes none, followed by the existing build commands.
- `docs/releasing.md` (new): how to cut a release (version in `Cargo.toml`, merge into `main`, push the tag, approve), what the workflow does, the set-up of D9, how to verify the attestation, and what to do when a release fails half way. Examples use `registry.example.com`.

### D11. Depot's runners for Open-Breach, GitHub's for any other owner

Added on 2026-11-24, on the owner's instruction: all builds of Open-Breach run on Depot.

Every job of every workflow in this repository chooses its runner by the owner of the repository, with the rule Trebuchet's workflows use:

```
runs-on: ${{ github.repository_owner == 'Open-Breach' && '<Depot label>' || '<GitHub label>' }}
```

| Jobs | Open-Breach | Any other owner |
|---|---|---|
| Compile Rust or build the image, `linux/amd64` (`check`, `image`) | `depot-ubuntu-24.04-4` | `ubuntu-24.04` |
| The same for `linux/arm64` (`image`) | `depot-ubuntu-24.04-arm-4` | `ubuntu-24.04-arm` |
| Small jobs (`workflows`, `manifest`, the rehearsal, `verify`, `publish`) | `depot-ubuntu-24.04` | `ubuntu-24.04` |

- *Why by owner:* a fork has no Depot runners. A job that names one would wait for a runner that never comes; with this rule a fork's checks run on GitHub's.
- *One label per job, never a list.* Depot's documentation selects a runner by one label. A list of labels asks GitHub for a runner that carries all of them, which is how self-hosted runners are selected by hand.
- The labels were checked against Depot's list of runner types on 2026-11-24. The plain label has two processors, `-4` has four.
- A job that calls another workflow has no runner of its own; the called jobs name theirs. The owner in the rule is the owner of the calling repository, which is the same one.
- `ubuntu-latest` is gone from the workflows. GitHub moves that label to a new Ubuntu from time to time, and Depot's label names the version.
- *Difference from Trebuchet:* its workflows also read a repository variable (`RUNNER_LINUX_X64`) that names another label. Butterbot does not. It has two sizes per platform, which one variable cannot express, and a variable would let the runner of the publishing job be changed without a change to the repository.
- A test reads `runs-on` of every job in every workflow and fails for a job that does not follow the rule, or that is new and has no size.

Consequences:

- The publishing job, and with it the registry login, runs on a machine of Depot, not of GitHub. See the risks.
- GitHub records a Depot runner as self-hosted, and so does the attestation of a release. `gh attestation verify` accepts that unless it is told not to (`--deny-self-hosted-runners`); `docs/releasing.md` says so.
- If Depot is not available, the jobs wait. Nothing switches back to GitHub's runners by itself.

## Risks / Trade-offs

- **The registry login may be able to overwrite other images in the registry.** → It is released to one job only, after all checks, after the owner's approval, for version tags only. The pipeline has a login of its own that can be withdrawn. The run summary and the attestation give a digest to compare the registry's content with.
- **The publishing job runs on a Depot runner (D11).** The registry login is then present on a machine of a third party, for the length of that job. → Accepted on the owner's instruction: Open-Breach builds on Depot throughout. The login is the pipeline's own and can be withdrawn on its own (D9).
- **Masking is GitHub's, not ours.** A tool could print the host in a form that is not the exact secret value. → The script does not transform the host, and the rehearsal checks that the script's own output does not contain it. The owner looks at the log of the first release (task 5.3).
- **An approved run executes whatever the tagged commit's workflow says.** A change to the release workflow could misuse the secrets once approved. → The tag must lie on `main`, which agents do not push to, and the owner approves only releases he started.
- **The rehearsal's registry is not the real one.** HTTPS, the real login and the environment are first exercised by the first release. → A missing or wrong setting fails before anything is pushed; a failure later can be run again (D6).
- **A public attestation shows that a release happened, with its digest and commit.** → Accepted. The repository and its tags are public anyway.
- **`latest` moves with every release that is not a pre-release**, also when an older line is released after a newer one. → Accepted for now; see the non-goals. Deployments should name a version or a digest.

## Migration Plan

Nothing running changes when this is merged. The first release is the owner's:

1. Set up the environment and the registry login (D9).
2. Merge `dev` into `main`. Check that `Cargo.toml` carries the version to release.
3. Push the tag, for example `v0.1.0`.
4. When the checks are green, approve the publishing job.
5. Compare the digest in the run summary with the registry, verify the attestation, and pull the image on the platform the cluster runs on.

To withdraw a release, delete its tags in the registry. Deployments that name another version or a digest are not affected.

## Open Questions

- Cleaning up old versions and, if they are used, per-platform tags in the registry. Not needed until the registry holds many.
- Whether a later public image, published through Trebuchet, replaces this workflow or runs beside it. To be decided with that change.
