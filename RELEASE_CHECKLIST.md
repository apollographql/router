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

In addition, you will find `docker` and `helm` assets:
 - [docker](https://github.com/apollographql/router/pkgs/container/nightly%2Frouter)
 - [helm](https://github.com/apollographql/router/pkgs/container/helm-charts-nightly%2Frouter)

This is a list of the things that need to happen during a release.

Build a Release
---------------

Most of this will be executing some simple commands but here's a high level understanding and some terminology.  There will be a total of 3 pull-requests involved:

- **a Release PR**: this will merge **into `main`**.  It will be a real merge commit and it **should NOT be squashed**.
- **a Release _Prep_ PR**: this will merge into the release PR _above_.  It **SHOULD be squashed**.
- **Reconciliation PR**: a PR that merges `main` back into `dev`.  It will be a real merge commit and it **should NOT be squashed**.

The examples below will use [the GitHub CLI (`gh`)](https://cli.github.com/) to simplify the steps.  We can automate it further in the future, but feels like the right level of abstraction right now.

A release can be cut from any branch, but we assume you'll be doing it from `dev`.  If you're just doing a release candidate, you can skip merging it back into `main`.

1. Make sure you have `cargo` installed on your machine and in your `PATH`.
2. Pick the version number you are going to release.  This project uses [Semantic Versioning 2.0.0](https://semver.org/), so analyze the existing changes in the `.changesets/` directory to pick the right next version.  (e.g., If there are `feat_` changes, it must be a minor version bump.  If there are `breaking_` changes, it must be a _major_ version bump).  **Do not release a major version without explicit agreement from core team members**.
3. Checkout the branch you want to cut from.  Typically, this is `dev`, but you could do this from another branch as well.

   ```
   git checkout dev
   ```

4. We'll set some environment variables for steps that follow this, to simplify copy and pasting.  Be sure to customize these for your own conditions, and **set the version you picked in the above step** as `APOLLO_ROUTER_RELEASE_VERSION`:

   ```
   APOLLO_ROUTER_RELEASE_VERSION=#.#.#
   APOLLO_ROUTER_RELEASE_GIT_ORIGIN=origin
   APOLLO_ROUTER_RELEASE_GITHUB_REPO=apollographql/router
   ```

5. Make sure you have the latest from the remote before releasing, ensuring you're using the right remote!

   ```
   git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}"
   ```

6. Create a new branch `#.#.#`.  (The `#.#.#` values should be this release's version, and it is perfectly acceptable to use prerelease semantics, e.g., a branch named `1.5.3-rc.9`).  To do this using the environment variable we just set, we'll just run the following from the same terminal:

   ```
   git checkout -b "${APOLLO_ROUTER_RELEASE_VERSION}"
   ```
7. Push this new branch to the appropriate remote.  We will open a PR for it **later**, but this will be the **base** for the PR created in the next step).  (And `--set-upstream` will of course track this locally.  This is commonly abbreviated as `-u`.)

   ```
   git push --set-upstream "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "${APOLLO_ROUTER_RELEASE_VERSION}"
   ```

8. Create _another_ new branch called `prep-#.#.#` off of `#.#.#`.  This branch will be used for bumping version numbers and getting review on the changelog.  We'll do this using the same environment variable, so you can just run:

   ```
   git checkout -b "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
   ```

9. On this new `prep-#.#.#` branch, run the release automation script using this command to use the environment variable set previously:

   > **Note**
   > For this command, `GITHUB_TOKEN` is **not used**, but it is still _required_ at the moment, so it's set here to `prep`.  This is a bug in the releasing script that needs to be changed.

   ```
   cargo xtask release prepare $APOLLO_ROUTER_RELEASE_VERSION
   ```

   Running this command will:

     - Bump the necessary versions to the version specified, including those in the documentation.
     - Migrate the current set of `/.changesets/*.md` files into `/CHANGELOG.md` using the version specified.
     - Run our compliance checks and update the `licenses.html` file as appropriate.
     - Ensure we're not using any incompatible licenses in the release.

10. **MANUALLY CHECK AND UPDATE** the `federation-version-support.mdx` to make sure it shows the version of Federation which is included in the `router-bridge` that ships with this version of Router.  This can be obtained by looking at the version of `router-bridge` in `apollo-router/Cargo.toml` and taking the number after the `+` (e.g., `router-bridge@0.2.0+v2.4.3` means Federation v2.4.3).

11. Now, review and stage he changes produced by the previous step.  This is most safely done using the `--patch` (or `-p`) flag to `git add` (`-u` ignores untracked files).

    ```
    git add -up .
    ```

12. Now commit those changes locally, using a brief message:

    ```
    git commit -m "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

13. (Optional) Make local edits to the newly rendered `CHANGELOG.md` entries to do some initial editoral.

    These things should typically be resolved earlier in the review process, but need to be double checked:

     - There are no breaking changes.
     - Entries are in categories (e.g., Fixes vs Features) that make sense.
     - Titles stand alone and work without their descriptions.
     - You don't need to read the title for the description to make sense.
     - Grammar is good.  (Or great! But don't let perfect be the enemy of good.)
     - Formatting looks nice when rendered as markdown and follows common convention.

14. Now push the branch up to the correct remote:

    ```
    git push --set-upstream "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

15. Programatically create a small temporary file called `this_release.md` with the changelog details of _precisely this release_ from the `CHANGELOG.md`:

    > Note: This file could totally be created by the `xtask` if we merely decide convention for it and whether we want it checked in or not.  It will be used again later in process and, in theory, by CI.  Definitely not suggesting this should live on as regex.

    ```
    perl -0777 \
      -sne 'print "$1\n" if m{
        (?:\#\s               # Look for H1 Markdown (line starting with "# ")
        \[v?\Q$version\E\]    # ...followed by [$version] (optionally with a "v")
                              #    since some versions had that in the past.
        \s.*?\n$)             # ... then "space" until the end of the line.
        \s*                   # Ignore PRE-entry-whitespace
        (.*?)                 # Capture the ACTUAL body of the release.  But do it
                              # in a non-greedy way, leading us to stop when we
                              # reach the next version boundary/heading.
        \s*                   # Ignore POST-entry-whitespace
        (?=^\#\s\[[^\]]+\]\s) # Once again, look for a version boundary.  This is
                              # the same bit at the start, just on one line.
      }msx' -- \
        -version="${APOLLO_ROUTER_RELEASE_VERSION}" \
        CHANGELOG.md >  this_release.md
    ```

16. Now, run this command to generate the header and the PR and keep them in an environment variable:

    ```
    apollo_prep_release_header="$(
    cat <<EOM
    > **Note**
    >
    > When approved, this PR will merge into **the \`${APOLLO_ROUTER_RELEASE_VERSION}\` branch** which will — upon being approved itself — merge into \`main\`.
    >
    > **Things to review in this PR**:
    >  - Changelog correctness (There is a preview below, but it is not necessarily the most up to date.  See the _Files Changed_ for the true reality.)
    >  - Version bumps
    >  - That it targets the right release branch (\`${APOLLO_ROUTER_RELEASE_VERSION}\` in this case!).
    >
    ---
    EOM
    )"
    apollo_prep_release_notes="$(cat ./this_release.md)"
    ```

17. Use the `gh` CLI to create the PR, using the previously-set environment variables:

    ```
    echo "${apollo_prep_release_header}\n${apollo_prep_release_notes}" | gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create -B "${APOLLO_ROUTER_RELEASE_VERSION}" --title "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}" --body-file -
    ```

18. Use the `gh` CLI to enable **auto-squash** (**_NOT_** auto-**_merge_**) on the PR you just opened:

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --squash --body "" -t "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}" --auto "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

19. 🗣️ **Solicit feedback from the Router team on the prep PR**

    Once approved, the PR will squash-merge itself into the next branch.

20. After the PR has auto-merged, change your local branch back to the _non-_prep branch, pull any changes you (or others) may have added on GitHub :

    ```
    git checkout "${APOLLO_ROUTER_RELEASE_VERSION}" && \
    git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}"
    ```

20. Now, from your local final release branch, open the PR from the branch the prep PR already merged into:

    ```
    apollo_release_pr_header="$(
    cat <<EOM

    > **Note**
    > **This particular PR should be true-merged to \`main\`.**

    This PR represents the merge to \`main\` of the v${APOLLO_ROUTER_RELEASE_VERSION} release.

    This PR is **primarily a merge commit**, so reviewing every individual commit shown below is **not necessary** since those have been reviewed in their own PR.

    **However!** Some things to review on this PR:

    - Does this PR target the right branch? (usually, \`main\`)
    - Are the appropriate **version bumps** and **release note edits** in the end of the commit list (or within the last few commits).  In other words, "Did the 'release prep' PR actually land on this branch?"

    If those things look good, this PR is good to merge.
    EOM
    )"
    echo "${apollo_release_pr_header}" | gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create -B "main" --title "release: v${APOLLO_ROUTER_RELEASE_VERSION}" --body-file -
    ```

21. Use the `gh` CLI to enable **auto-merge** (**_NOT_** auto-**_squash_**):

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --merge --body "" -t "release: v${APOLLO_ROUTER_RELEASE_VERSION}" --auto "${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

22. 🗣️ **Solicit approval from the Router team, wait for the PR to pass CI and auto-merge into `main`**

23. After the PR has merged to `main`, pull `main` to your local terminal, and Git tag & push the release:

    This process will kick off the bulk of the release process on CircleCI, including building each architecture on its own infrastructure and notarizing the macOS binary.

    ```
    git checkout main && \
    git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" && \
    git tag -a "v${APOLLO_ROUTER_RELEASE_VERSION}" -m "${APOLLO_ROUTER_RELEASE_VERSION}" && \
    git push "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "v${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

24. Open a PR that reconciles `dev` (Make sure to merge this reconciliation PR back to dev, do not squash or rebase):

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create --title "Reconcile \`dev\` after merge to \`main\` for v${APOLLO_ROUTER_RELEASE_VERSION}" -B dev -H main --body "Follow-up to the v${APOLLO_ROUTER_RELEASE_VERSION} being officially released, bringing version bumps and changelog updates into the \`dev\` branch."
    ```

25. 👀 Follow along with the process by [going to CircleCI for the repository](https://app.circleci.com/pipelines/github/apollographql/router) and clicking on `release` for the Git tag that appears at the top of the list.  **Wait for `publish_github_release` to finish on this job before continuing.**

26. After the CI job has finished for the tag, re-run the `perl` command from Step 15, which will regenerate the `this_release.md` with changes that happened in the release review.

27. Change the links from `[@username](https://github.com/username)` to `@username` (TODO: Write more `perl` here. 😄)

    This ensures that contribution credit is clearly displayed using the user avatars on the GitHub Releases page when the notes are published in the next step.

28. Update the release notes on the now-published [GitHub Releases](https://github.com/apollographql/router/releases) (this needs to be moved to CI, but requires `this_release.md` which we created earlier):

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" release edit v"${APOLLO_ROUTER_RELEASE_VERSION}" -F ./this_release.md
    ```

29. Publish the Crate from your local computer from the `main` branch (this also needs to be moved to CI, but requires changing the release containers to be Rust-enabled and to restore the caches):

    ```
    cargo publish -p apollo-router
    ```

30. (Optional) To have a "social banner" for this release, run [this `htmlq` command](https://crates.io/crates/htmlq) (`cargo install htmlq`, or on MacOS `brew install htmlq`; its `jq` for HTML), open the link it produces, copy the image to your clipboard:

    ```
    curl -s "https://github.com/apollographql/router/releases/tag/v${APOLLO_ROUTER_RELEASE_VERSION}" | htmlq 'meta[property="og:image"]' --attribute content
    ```

### prep PR Review

Most review comments for the prep PR will be about the changelog. Once the prep PR is finalized and approved:

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
