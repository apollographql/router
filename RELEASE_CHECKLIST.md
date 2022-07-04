Release Checklist
=================

This is a list of the things that need to happen during a release.

Build a Release
---------------

### Prepare the Changelog (Full release only)

If you are releasing a beta or a release candidate, no official changelog is
needed, but you're not off the hook! You'll need to write testing instructions
in lieu of an official changelog.

1. Open the associated milestone. All issues and PRs should be closed. If
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
5. Update `docker.mdx` and `kubernetes.mdx` with the release version.
6. Update `helm/chart/router/Chart.yaml` and in `helm/chart/router/README.md` as follows:
   - increment the version. e.g. `version: 0.1.2` becomes `version: 0.1.3`
   - update the appVersion to the release version. e.g.: `appVersion: "v0.9.0"`
7. Update `federation-version-support.mdx` with the latest version info. Use https://github.com/apollographql/version_matrix to generate the version matrix.
8. Update the version in docker-compose files in `dockerfiles` directory.

### Start a release PR

1. Make sure you have `cargo` installed on your machine and in your `PATH`.
2. Create a new branch "#.#.#" where "#.#.#" is this release's version
    (release) or "#.#.#-rc.#" (release candidate)
3. Update the version in `*/Cargo.toml` (do not forget the ones in scaffold templates).
4. Add a new section in `CHANGELOG.md` with the contents of `NEXT_CHANGELOG.md`
5. Put a Release date and the version number on the new `CHANGELOG.md` section
4. Update the version in `NEXT_CHANGELOG.md`.
5. Clear `NEXT_CHANGELOG.md` leaving only the template.
6. Run `cargo check` so the lock file gets updated.
7. Run `cargo xtask check-compliance`.
8. Push up a commit with the `*/Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and
    `NEXT_CHANGELOG.md` changes. The commit message should be "release: v#.#.#" or
    "release: v#.#.#-rc.#"
9. Request review from the Router team.

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
