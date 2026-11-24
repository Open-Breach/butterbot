# Tasks

## 1. CI hands its images on

- [ ] 1.1 Make `ci.yml` callable from another workflow (`workflow_call`) and restrict its `push` trigger to branches; verify with a test in `tests/packaging.rs` that reads the workflow's triggers, and by the checks of the pull request still running every job
- [ ] 1.2 In each `image` job, save the image the container tests ran against and upload it as the artifact `image-linux-amd64` or `image-linux-arm64` with a short retention; verify in the run of the pull request that both artifacts exist and that a loaded archive has the identifier of the image that was tested
- [ ] 1.3 Add a workflow linter to the checks (for example `actionlint`); verify it passes on the existing workflow and fails on a deliberately broken copy
- [ ] 1.4 Run every job of every workflow on Depot's runners when the repository belongs to Open-Breach and on GitHub's runner of the same platform for any other owner, with one label per job and the sizes of design D11; verify with a test in `tests/packaging.rs` that reads `runs-on` of every job, and in the run of the pull request that every job was given a Depot runner

## 2. Publishing script and rehearsal

- [ ] 2.1 Write the publishing script under `.github/scripts/` with its inputs (registry host, login, version, the two image archives) and the missing-setting check that names the missing one and prints no value; verify with a test that runs it once per missing setting and checks the message, the exit status and that no registry was contacted
- [ ] 2.2 Add the rehearsal job to `ci.yml`: a throwaway registry with a login generated in the job, the two image artifacts of the run, a made-up version; verify that it runs without any repository secret and that the script can log in and push to it
- [ ] 2.3 Implement pushing both platform images, creating the multi-platform image and setting the tags of a release, with the tags set last; verify in the rehearsal that `X.Y.Z`, `X.Y` and `latest` name one image listing exactly `linux/amd64` and `linux/arm64`, that pulling for each platform yields that platform's image, and that each published image has the identifier of the tested one
- [ ] 2.4 Implement the pre-release rule; verify in the rehearsal that releasing `0.2.0-rc.1` after `0.1.0` sets only `0.2.0-rc.1` and leaves `latest` and `0.1` where they were
- [ ] 2.5 Implement the check of an existing version; verify in the rehearsal that a different build of a released version is refused with nothing pushed, and that a second run with the same images completes and leaves the tags unchanged
- [ ] 2.6 Verify in the rehearsal that a missing archive for one platform fails the script before any tag of the release is set or moved
- [ ] 2.7 Write the version, the tags and the digest to the run summary, naming the image as `butterbot`, and expose the digest to later steps; verify in the rehearsal that the summary has all three, and that nothing the script itself writes contains the registry host it was given
- [ ] 2.8 Document in `docs/releasing.md` what the publishing steps do and how the platform images reach the registry (per-platform tags or by digest); verify the text matches what the rehearsal observes

## 3. Release workflow

- [ ] 3.1 Write the script that checks a tag (form `v<semantic version>`, version equal to the program's as Cargo reports it, commit contained in `main`); verify with tests in a temporary repository: a matching tag passes, a different version fails naming both versions, a commit outside `main` fails saying so, and `v1` or `vfoo` fails
- [ ] 3.2 Add the release workflow: started only by tags matching `v*`, a concurrency group, the jobs `verify`, `checks` (the CI workflow) and `publish` in that order; verify with the workflow linter and with tests in `tests/packaging.rs` that read the triggers and the job order
- [ ] 3.3 Give `publish` the environment `release` and the three secrets, as the only job in any workflow that has either, and keep every other job at read-only permissions; verify with a test in `tests/packaging.rs` that no other job in any workflow refers to the environment or to a `REGISTRY_` secret, and that no workflow or script contains a registry host other than `registry.example.com` or `localhost`
- [ ] 3.4 Read the current documentation of GitHub's attestation action and of `gh attestation verify` for images, then add the attestation step for the digest of the multi-platform image with the subject name `butterbot`, not pushed to the registry; verify with a test that the step's subject name is the literal `butterbot` and that it takes the digest from the publishing step. If the registry's host would have to appear in the attestation, leave this task open and report (design D7)
- [ ] 3.5 Write the rest of `docs/releasing.md`: cutting a release, the set-up outside the repository (environment, reviewer, tag rule, the three secrets, a login for the pipeline), verifying the attestation, and what to do when a release fails half way; verify every command in it is spelled as the tools accept it and that every example uses `registry.example.com`

## 4. What the project offers

- [ ] 4.1 Update `README.md`: the program is built from source, the project publishes no image, a short note that it packs into a container with a pointer to `docs/deployment.md`, and `docs/releasing.md` in the list of documents; verify with the existing README link test and a new test for the two statements
- [ ] 4.2 Update `docs/deployment.md`: replace "No image is published yet" with the statement that the project publishes none, keeping the build commands; verify the commands still run as written

## 5. Checks

- [ ] 5.1 Run `openspec validate add-image-release --strict` and confirm that every scenario of the spec has a test or is listed as checked by the owner at the first release; verify by listing the scenario-to-test mapping in the pull request description
- [ ] 5.2 Set up the environment `release` (required reviewer, only tags matching `v*`, the three secrets) and a login for the pipeline at the registry; verify that the environment's page shows the reviewer, the rule and three secrets (the owner does this; nothing about it is written into the repository)
- [ ] 5.3 Cut the first release: push the version tag, approve, then check that the run's log shows the registry masked, that the registry holds the tags with the digest from the summary, that `gh attestation verify` succeeds, and that the image pulls on the cluster's platform; verify by adding a row to the "Verified against" table in `docs/deployment.md` that does not name the registry (the owner does this)

## Workflow follow-up

- This change adds a capability of its own and modifies none, so it can be archived independently of the other open changes.
- A public image, if it is offered later, is a change of its own and goes through Trebuchet.
