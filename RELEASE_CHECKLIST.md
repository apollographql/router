Release Checklist
=================

This is a list of the things that need to happen during a release.

Build a Release
---------------

### Prereqs

1. Make sure you have `cargo` installed on your machine and in your `PATH`.  Run `command -v cargo` to verify it is available.
2. These instructions will assume that you have a Git remote named `origin` which points to the GitHub repository that has the source code the release will be done from.  **If you use a Git remote other than `origin`, use that name instead in the following instructions.**  Run `git remote -v` to see the configuration of your Git remotes.
3. Install `helm-docs` if it is not installed already by following the instructions [on their repository](https://github.com/norwoodj/helm-docs/#Installation).  Run `command -v helm-docs` to confirm that it is already installed.

### Overview

1. Determine the version bump for the release
1. Create the release branch for later steps.
2. Release preparation PR
    * This PR will consist of version bumps and CHANGELOG changes.
3. Release PR
    * This PR will be the merge commit from the release branch into the `main` branch.
4. Performing the release
   * This step consists of running commands that start the actual release on CircleCI that releasing the packages onto GitHub Releases, and a follow-up step which publishes the packages to Crates.io.
5. Reconciling the `dev` branch
   * This will consist of a PR that merges `main` back into `dev` after the release.

### Determine the version bump for the release

This project uses the Semantic Versioning Specification v2.0.0 for its version numbers.

1. Open the `NEXT_CHANGELOG.md` and analyze the changes:

    1. If this is a follow-up to a previous "prerelease" (e.g., alpha, beta), then it might be sufficient to merely increment the prerelease identifier (i.e., `alpha.0` becomes `alpha.1`).
    1. If there are "Breaking Changes", then the _major_ version should be bumped.  Keep in mind that major versions must be agreed to by the entire team ahead of time.
    2. If there are entries in the "Features" section, then the _minor_ version should be bumped.
    3. In all other cases, a _patch_ version bump will be sufficient.

2. If this is going to be a prerelease version, a prerelease identifier will be added to the end of the version number.  For example, `-alpha.0` or

### Create the release branch

We won't use the release branch right away, but instead use it as the target for the release preparation PR.  In a later step, the release branch will be a _different_ PR that lands into `main`.  Create the branch and push it so it can be used for the following step:

1. Ensure that you have the latest reference to the current `dev` on your local machine:

    ```
    git fetch origin
    ```

2. Create a branch named `#.#.#`

### Create a release preparation PR

The release preparation PR

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
11. Update the license list with `cargo about generate --workspace -o licenses.html about.hbs`.
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
