# Spec Delta

## Purpose

Defines how a released version of Butterbot becomes a container image in a private registry: what starts a release, what is checked first, what is published under which tags, and how the registry stays out of the public repository.

## ADDED Requirements

### Requirement: A version tag starts a release
A release SHALL start when a tag of the form `v<version>` is pushed, where `<version>` is a semantic version such as `0.1.0` or `0.2.0-rc.1`. Nothing else SHALL start a release: not a push to a branch, not a pull request, and not a tag of another form.

#### Scenario: Version tag
- **WHEN** the tag `v0.1.0` is pushed
- **THEN** a release of version `0.1.0` starts

#### Scenario: Change to a branch
- **WHEN** a commit is pushed to a branch or a pull request is opened
- **THEN** no release starts

#### Scenario: Another tag
- **WHEN** the tag `experiment-1` is pushed
- **THEN** no release starts

### Requirement: The tag matches the program and lies on main
Before anything is built, a release SHALL fail when the version in the tag differs from the version of the program at the tagged commit, or when the tagged commit is not contained in the `main` branch. The failure message SHALL say which of the two it is.

#### Scenario: Version differs
- **WHEN** the tag `v0.2.0` is pushed on a commit whose program version is `0.1.0`
- **THEN** the release fails with a message naming both versions, and nothing is published

#### Scenario: Commit outside main
- **WHEN** a version tag is pushed on a commit that is not contained in `main`
- **THEN** the release fails with a message saying so, and nothing is published

### Requirement: Only a checked image is published
A release SHALL run, on the tagged commit, every check that runs on a change, including the container tests on both platforms, and SHALL publish nothing unless all of them pass. The image published for a platform SHALL be the image those container tests ran against, not a second build.

#### Scenario: A check fails
- **WHEN** a test fails on the tagged commit
- **THEN** nothing is pushed to the registry

#### Scenario: The tested image is the published image
- **WHEN** a release publishes the image for `linux/arm64`
- **THEN** its image identifier equals that of the image the container tests ran against on `linux/arm64`

### Requirement: One image name for both platforms
A release SHALL publish images for `linux/amd64` and `linux/arm64`, each built on a machine of its own platform, under one image name, so that pulling a tag yields the image for the platform of the machine that pulls.

#### Scenario: Both platforms listed
- **WHEN** the tag `0.1.0` of the published image is inspected
- **THEN** it lists exactly the platforms `linux/amd64` and `linux/arm64`

#### Scenario: Pulled for arm64
- **WHEN** the tag `0.1.0` is pulled for `linux/arm64`
- **THEN** the image received is the one that was built on the arm64 machine

### Requirement: Tags of a release
For a version `X.Y.Z` a release SHALL set the tags `X.Y.Z`, `X.Y` and `latest`, all naming the same multi-platform image. For a pre-release version, one with a suffix after `X.Y.Z`, it SHALL set only the tag of the full version. These tags SHALL be set only after the images of both platforms are in the registry.

#### Scenario: Release
- **WHEN** version `0.1.0` is released
- **THEN** the tags `0.1.0`, `0.1` and `latest` name the same image

#### Scenario: Pre-release
- **WHEN** version `0.2.0-rc.1` is released while `latest` names version `0.1.0`
- **THEN** only the tag `0.2.0-rc.1` is set, and `latest` and `0.1` still name version `0.1.0`

#### Scenario: One platform is missing
- **WHEN** the image of one platform is missing or cannot be pushed
- **THEN** none of the release's tags is set or moved

### Requirement: A platform's image may have a tag of its own
To get the image of each platform into the registry, a release MAY first push it under a tag of its own: the full version followed by `-linux-amd64` or `-linux-arm64`. These are not tags of the release: they name the image of one platform, and nothing refers to them afterwards. A version that itself ends in `-linux-amd64` or `-linux-arm64` SHALL be refused before anything is pushed, because its tag would be such a tag of another version.

#### Scenario: Tags of the platforms
- **WHEN** version `0.1.0` is released
- **THEN** the registry holds no tag for it other than `0.1.0`, `0.1`, `latest`, `0.1.0-linux-amd64` and `0.1.0-linux-arm64`

#### Scenario: Version that ends like the tag of a platform's image
- **WHEN** the version `0.1.0-linux-amd64` is given to the publishing steps
- **THEN** they fail with a message saying so, before they log in to the registry

### Requirement: A released version is not overwritten
Before pushing anything, a release SHALL fail when the registry already holds the tag of the full version and that tag names images other than the ones this release is about to publish. When it names exactly these images, the release SHALL continue, so that running a failed publishing job again completes the release.

#### Scenario: Version already released from another build
- **WHEN** version `0.1.0` is in the registry and a different build of `0.1.0` is released
- **THEN** the release fails with a message saying that the version exists, and the registry is unchanged

#### Scenario: Failed job run again
- **WHEN** the publishing job failed after setting the tag `0.1.0` and is run again with the same images
- **THEN** it completes, and the tags name the same images as before

### Requirement: Registry settings come from the environment's secrets
The registry's host, the login and its password SHALL be read from the secrets `REGISTRY_HOST`, `REGISTRY_USERNAME` and `REGISTRY_PASSWORD` of the deployment environment `release`, and from nowhere else. The image SHALL be published as `<REGISTRY_HOST>/butterbot`.

#### Scenario: Image name
- **WHEN** a release publishes with `REGISTRY_HOST` set to `registry.example.com`
- **THEN** the image is `registry.example.com/butterbot`

#### Scenario: Repository contents
- **WHEN** the workflows and scripts of the repository are read
- **THEN** the host, the login and the password of the registry appear only as references to the three secrets

### Requirement: Nothing else names the registry
No file in the repository, run summary, artifact or attestation SHALL contain the registry's host or login. The workflow SHALL hand both on only as secrets and SHALL NOT transform or print them, so that the platform masks them wherever a tool writes them to the log.

#### Scenario: Run summary
- **WHEN** a release has finished
- **THEN** its summary names the image as `butterbot` and does not contain the registry's host

#### Scenario: Output of the publishing steps
- **WHEN** the publishing steps run with a registry host given to them
- **THEN** nothing they write themselves, apart from the output of the tools they call, contains that host

### Requirement: A missing registry setting stops the release
When one of the three registry secrets is missing or empty, the publishing job SHALL fail with a message that names the missing secret, before it contacts a registry. This check SHALL be the first of the publishing steps. It SHALL never print a secret's value.

#### Scenario: Environment not set up
- **WHEN** a release reaches the publishing job and `REGISTRY_PASSWORD` is not set
- **THEN** the job fails with a message naming `REGISTRY_PASSWORD`, and nothing is pushed

### Requirement: Publishing waits behind one approval
Exactly one job SHALL read the registry secrets. It SHALL run in the deployment environment `release` and only after every check of the release has passed, so that a required reviewer set on that environment approves a release once, with the results of the checks in view. No job that runs for a branch or a pull request SHALL refer to that environment.

#### Scenario: Release waits
- **WHEN** the checks of a release have passed and the environment has a required reviewer
- **THEN** the run waits at the publishing job until the reviewer approves, and nothing is pushed before that

#### Scenario: Pull request
- **WHEN** the checks run for a pull request
- **THEN** no job has access to the registry secrets

### Requirement: The release is attested
After publishing, a release SHALL record a build provenance attestation with GitHub for the digest of the published multi-platform image. The subject name of the attestation SHALL be `butterbot`. It SHALL NOT contain the registry's host, because attestations of a public repository are written to a public log.

#### Scenario: Attestation recorded
- **WHEN** a release has finished
- **THEN** an attestation exists for the digest named in the run's summary, with the subject name `butterbot`

#### Scenario: Verification
- **WHEN** the owner verifies the published image against the repository with the GitHub command line tool
- **THEN** the verification succeeds

### Requirement: The run reports what it published
The summary of a release run SHALL state the version, the tags that were set, and the digest of the multi-platform image, so that a deployment can refer to the image by digest and the registry's content can be compared with the run.

#### Scenario: Summary of a release
- **WHEN** version `0.1.0` has been released
- **THEN** the run's summary shows `0.1.0`, the tags `0.1.0`, `0.1` and `latest`, and one digest

### Requirement: Publishing is rehearsed on every change
The checks of every change SHALL run the publishing steps of a release against a throwaway registry that requires a login and exists only for that run, with the images built for that change, and SHALL verify the outcome. The rehearsal SHALL use no secret of the repository and SHALL record no attestation.

#### Scenario: Rehearsal
- **WHEN** the checks run for a pull request
- **THEN** the publishing steps run against a throwaway registry, and the tags and platforms they produce are verified

#### Scenario: Publishing steps broken
- **WHEN** a change breaks the publishing steps
- **THEN** the checks of that change fail

### Requirement: The project publishes no image
The repository's documents SHALL present Butterbot as a console application that is built from source, SHALL state that the project publishes no container image, and SHALL give the commands for building an image from the `Dockerfile`. The repository SHALL contain no workflow that publishes an image to a public registry.

#### Scenario: Reader of the README
- **WHEN** the README is read
- **THEN** it says how to build the program from source, says that no image is published, and points to the commands for building one

#### Scenario: Workflows
- **WHEN** the workflows of the repository are read
- **THEN** none of them names a public registry as a place to publish to
