Release Checklist
=================

Nightly Releases
----------------

As of the introduction of [PR #2409](https://github.com/apollographql/router/pull/2409), nightly releases are automatically built on a daily basis.  This is accomplished automatically through use of a parameterized invocation of the [`nightly` workflow](https://github.com/apollographql/router/blob/HEAD/.circleci/config.yml#L704-L711) using [CircleCI's Scheduled Pipelines](https://circleci.com/docs/scheduled-pipelines/) feature.

### One-off builds

In the way the schedule is defined, nightly builds are done from the `dev` branch.  However, the functionality that powers nightly builds can be used to also build from _any_ branch (including PRs) and produce a pre-release, "nightly style" build from any desired commit.

This process can only be done by members of the Apollo Router `router` GitHub repository with contributor permissions on CircleCI.

To invoke a one-off `nightly` build:

1. Go to the CircleCI Pipelines view for this repository](https://app.circleci.com/pipelines/github/apollographql/router)
2. Click on the **"All Branches"** drop-down menu and choose a branch you'd like to build from.
3. Press the **"Trigger Pipeline"** button in the top-right of the navigation (to the left of the "Project Settings" button).
4. Expand the "Add Parameters" section.
5. Add one parameter using the following configuration:

   **Parameter type:** `boolean`
   **Name:** `nightly`
   **Value:** `true`
6. Press **"Trigger Pipeline"**
7. Wait a couple seconds for the pipeline to begin and show in the list.

To obtain the binary builds from the pipeline which was launched:

> **Note**
> Built nightlies are only available on the Artifacts for a job within 30 days after the CircleCI pipeline that created them is finished.  If you need them after this period, you will need to re-run the pipeline and wait for it to finish again.  You can do this by clicking the "Rerun from start" option on the pipeline.

1. Click on the workflow name: **`nightly`** of the newly launched pipeline.  In the above steps, this is the pipeline that appeared after step 7.
2. Click on the job representing the system architecture you'd like to obtain the build binary for.  For example, to get the macOS binary, click on `build_release-macos_build`.
3. If the job hasn't already finished successfully, **wait for the job to finish successfully**.
4. Click on the **Artifacts** tab.
5. Click on the link to the `.tar.gz` file to download the tarball of the build distribution.  For example, you might click on a link called `artifacts/router-v0.0.0-nightly-20230119-abcd1234-x86_64-apple-darwin.tar.gz` for a macOS build done on the 19th of January 2023 from commit hash starting with `abcd1234`.

This is a list of the things that need to happen during a release.

Build a Release
---------------

### Prepare the Changelog (Full release only)

If you are releasing a beta or a release candidate, no official changelog is
needed, but you're not off the hook! You'll need to write testing instructions
in lieu of an official changelog.

1. Open the associated GitHub milestone for the release. All issues and PRs should be closed. If
    they are not you should reassign all open issues and PRs to future
    milestones.
2. Go through the commit history since the last release. Ensure that all PRs
    that have landed are marked with the milestone. You can use this to
    show all the PRs that are merged on or after YYYY-MM-DD:
    `https://github.com/issues?utf8=%E2%9C%93&q=repo%3Aapollographql%2Frouter+merged%3A%3E%3DYYYY-MM-DD`
3. Go through the closed PRs in the milestone. Each should have a changelog
    label indicating if the change is documentation, feature, fix, or
    maintenance. If there is a missing label, please add one. If it is a
    breaking change, also add a BREAKING label.
4. Set the release date in `NEXT_CHANGELOG.md`. Add this release to the
    `CHANGELOG.md`. Use the structure of previous entries.

### Start a release PR

1. Make sure you have `cargo` installed on your machine and in your `PATH`.
2. Create a new branch "#.#.#" where "#.#.#" is this release's version
    (release) or "#.#.#-rc.#" (release candidate)
3. Update the `version` in `*/Cargo.toml` (do not forget the ones in scaffold templates).
4. Update the `apollo-router` Git tags in the `dependencies` sections of the `Cargo.toml` files in `apollo-router-scaffold/templates/**`.
5. Update the `PACKAGE_VERSION` value in `scripts/install.sh` (it should be prefixed with `v`!)
6. Update `docker.mdx` and `kubernetes.mdx` with the release version.
7. Update `helm/chart/router/Chart.yaml` as follows:
   - update the version and the appVersion to the release version. e.g.: `appVersion: "v0.9.0"`
8 Update `helm/chart/router/README.md` by running this from the repo root: `(cd helm/chart && helm-docs router)`.
  (If not installed, you should [install `helm-docs`](https://github.com/norwoodj/helm-docs))
9. Update the kubernetes section of the docs:
  - go to the `helm/chart/router` folder
  - run 
  ```helm template --set router.configuration.telemetry.metrics.prometheus.enabled=true  --set managedFederation.apiKey="REDACTED" --set managedFederation.graphRef="REDACTED" --debug .```
  - Paste the output in the `Kubernetes Configuration` example of the `docs/source/containerization/kubernetes.mdx` file
9. Update `federation-version-support.mdx` with the latest version info. Use https://github.com/apollographql/version_matrix to generate the version matrix.
10. Update the `image` of the Docker image within `docker-compose*.yml` files inside the `dockerfiles` directory.
11. Update the license list with `cargo xtask licenses`.
    (If not installed, you can install `cargo-about` by running `cargo install cargo-about`.)
12. Add a new section in `CHANGELOG.md` with the contents of `NEXT_CHANGELOG.md`
13. Put a Release date and the version number on the new `CHANGELOG.md` section
14. Update the version in `NEXT_CHANGELOG.md`.
15. Clear `NEXT_CHANGELOG.md` leaving only the template.
16. Run `cargo check` so the lock file gets updated.
17. Run `cargo xtask check-compliance`.
18. Push up a commit with all the changes. The commit message should be "release: v#.#.#" or "release: v#.#.#-rc.#"
19. Request review from the Router team.

### Review

Most review comments will be about the changelog. Once the PR is finalized and
approved:

1.  Always use `Squash and Merge` GitHub button.

### Tag and build release

This part of the release process is handled by CircleCI, and our binaries are
distributed as GitHub Releases. When you push a version tag, it kicks off a
workflow that checks out the tag, builds release binaries for multiple
platforms, and creates a new GitHub release for that tag.

1.  Wait for tests to pass.
2.  Have your PR merged to `main`.
3.  Once merged, run `git checkout main` and `git pull`.
4.  Sync your local tags with the remote tags by running
    `git tag -d $(git tag) && git fetch --tags`
5.  Tag the commit by running either `git tag -a v#.#.# -m "#.#.#"` (release),
    or `git tag -a v#.#.#-rc.# -m "#.#.#-rc.#"` (release candidate)
6.  Run `git push --tags`.
7.  Wait for CI to pass.

### Edit the release

After CI builds the release binaries, a new release will appear on the
[releases page](https://github.com/apollographql/router/releases), click
`Edit`, update the release notes, and save the changes to the release.

#### If this is a stable release (not a release candidate)

1. Paste the current release notes from `NEXT_CHANGELOG.md` into the release body.
2. Reset the content of `NEXT_CHANGELOG.md`.

#### If this is a release candidate

1.  CI should already mark the release as a `pre-release`. Double check that
    it's listed as a pre-release on the release's `Edit` page.
2.  If this is a new rc (rc.0), paste testing instructions into the release
    notes.
3.  If this is a rc.1 or later, the old release candidate testing instructions
    should be moved to the latest release candidate testing instructions, and
    replaced with the following message:

    ```markdown
    This beta release is now out of date. If you previously installed this
    release, you should reinstall and see what's changed in the latest
    [release](https://github.com/apollographql/router/releases).
    ```

    The new release candidate should then include updated testing instructions
    with a small changelog at the top to get folks who installed the old
    release candidate up to speed.

### Publish the release to Crates.io

0. **To perform these steps, you'll need access credentials which allow you publishing to Crates.io.**
1. Make sure you are on the Git tag you have published and pushed in the previous step by running `git checkout v#.#.#` (release) or `git checkout v#.#.#-rc.#` (release candidate).  (You are probably still on this commit)
2. Change into the `apollo-router/` directory at the root of the repository.
3. Make sure that the `README.md` in this directory is up to date with any necessary or relevant changes.  It will be published as the crates README on Crates.io.
4. Run `cargo publish --dry-run` if you'd like to smoke test things
5. Do the real publish with `cargo publish`.

Troubleshooting a release
-------------------------

Mistakes happen. Most of these release steps are recoverable if you mess up.

### I pushed the wrong tag

Tags and releases can be removed in GitHub. First,
[remove the remote tag](https://stackoverflow.com/questions/5480258/how-to-delete-a-remote-tag):

```console
git push --delete origin vX.X.X
```

This will turn the release into a `draft` and you can delete it from the edit
page.

Make sure you also delete the local tag:

```console
git tag --delete vX.X.X
```
