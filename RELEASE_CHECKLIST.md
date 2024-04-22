Release Checklist
=================

## Table of Contents

- [Building a Release](#building-a-release)
  - 👀 [Before you begin](#before-you-begin)
  - [Starting a release PR](#starting-a-release-pr) - The release PR tracks a branch and gathers commits, allows changelog review.
  - _(optional)_ [Cutting a pre-release](#cutting-a-pre-release) (i.e., release candidate, alpha or beta) - Near-final versions for testing.
  - [Preparing the final release](#preparing-the-final-release) - Final version number bumps and final changelog preparation.
  - [Finishing the release](#finishing-the-release) - Builds the final release, merges into `main`, reconcile `dev` trunk.
  - Verifying the release (TODO)
  - [Troubleshooting a release](#troubleshooting-a-release) - Something went wrong?
- [Nightly releases](#nightly-releases)

## Building a Release

There are different types of releases:

- **General release**

  These releases are typically identified with a `x.y.z` semver identifier.  (e.g., `1.42.0`).
- **Pre-release**

  Such a release could be a "release candidate", or an alpha/beta release and would use a `x.y.z-pre.w` identifier (e.g., `1.42.0-alpha.0`).
- **Nightly releases**

  These are special releases cut from a particular Git commit.  Despite their name, they don't necessarily have to be done at a nightly interval.  They are identified by an identifier of `v0.0.0-nightly-YYYYMMDD-COMMITHASH`.  More details on these is found in the dedicated [Nightly releases] section.

  [Nightly releases]: #nightly-releases

The process is **not fully automated**, but the release consists of copying and pasting commands that do all the work for you.  Here's a high level understanding and some terminology, which will help so you can understand some key components:

- Pull Requests
  - There will be a total of 3 pull-requests involved:
    - **a Release "Staging" PR**: this will merge **into `main`**.  When it merges it will be a real merge commit and it **should NOT be squashed**.  It starts off as a draft, and graduates to a "ready for review" PR once any pre-release versions are issued and after preparation is done.
    - **a Release _Prep_ PR**: this will merge into the release PR _above_.  It **SHOULD be squashed**.  The release preparation PR is only done just before the final release and _**after** any prereleases_.
    - **Reconciliation PR**: a PR that merges `main` back into `dev` after the final release is done.  It will be a real merge commit and it **should NOT be squashed**.
- Peer Reviews
  - The actual code being released will have been reviewed in other PRs.
  - The "Release Prep" PR is reviewed

The examples below will use [the GitHub CLI (`gh`)](https://cli.github.com/) to simplify the steps.  We can automate it further in the future, but feels like the right level of abstraction right now.

### Before you begin

#### Software requirements

Make sure you have the following software installed and available in your `PATH`.

  - `gh`: [The GitHub CLI](https://cli.github.com/)
  - `cargo`: [Cargo & Rust Installation](https://doc.rust-lang.org/cargo/getting-started/installation.html)
  - `helm`: see <https://helm.sh/docs/intro/install/>
  - `helm-docs`: see <https://github.com/norwoodj/helm-docs#installation>
  - `cargo-about`: install with `cargo install --locked cargo-about`
  - `cargo-deny`: install with `cargo install --locked cargo-deny`
  - `set-version` from `cargo-edit`: `cargo install --locked cargo-edit`

#### Pick a version

This project uses [Semantic Versioning 2.0.0](https://semver.org/).  When releasing, analyze the existing changes in the [`.changesets/`](./.changesets) directory to pick the right next version:

- If there are `feat_` changes, it must be a _semver-minor_ version bump.
- If there are `breaking_` changes, it must be a _semver-major_ version bump.  **Do not release a major version without explicit agreement from core team members**.
- In all other cases, you can release a _semver-patch_ version.

> **Note**
> The full details of the `.changesets/` file-prefix convention can be found [its README](.changesets/README.md#conventions-used-in-this-changesets-directory).

### Starting a release PR

Creating a release PR is the first step of starting a release, whether there will be pre-releases or not.  About a release PR:

* A release PR is based on a release branch and a release branch gathers all the commits for a release.
* The release PR merges into `main` at the time that the release becomes official.
* A release can be started from any branch or commit, but it is almost always started from `dev` as that is the main development trunk of the Router.
* The release PR is in a draft mode until after the preparation PR has been merged into it.

Start following the steps below to start a release PR.  The process is **not fully automated**, but largely consists of copying and pasting commands that do all the work for you.  The descriptions above each command explain what the command aims to do.

1. Make sure you have all the [Software Requirements](#software-requirements) above fulfilled.

2. Ensure you have decided the version using [Pick a version](#pick-a-version).

3. Checkout the branch or commit you want to cut from.  Typically, this is `dev`, but you could do this from another branch as well:

   ```
   git checkout dev
   ```

4. We'll set some environment variables for steps that follow this, which will enable copying-and-pasting subsequent steps.  Customize these for your own conditions, **set the version you picked in the above step** as `APOLLO_ROUTER_RELEASE_VERSION`, and then paste into your terminal (press enter to complete it):

   > **Note**
   > You should **not** fill in `APOLLO_ROUTER_PRERELEASE_SUFFIX` at this time.  Visit [Cutting a pre-release](#cutting-a-pre-release) after opening the original release PR.

   ```
   APOLLO_ROUTER_RELEASE_VERSION="#.#.#"                  # Set me!
   APOLLO_ROUTER_RELEASE_GIT_ORIGIN=origin
   APOLLO_ROUTER_RELEASE_GITHUB_REPO=apollographql/router
   APOLLO_ROUTER_PRERELEASE_SUFFIX=""                     # Intentionally blank.
   ```

5. Make sure you have the latest from the remote before releasing, ensuring you're using the right remote!

   ```
   git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}"
   ```

6. Create a new branch `#.#.#` from the current branch which will act as the release branch.  (The `#.#.#` values should be this release's version.  To do this using the environment variable we just set, we'll just run the following from the same terminal:

   ```
   git checkout -b "${APOLLO_ROUTER_RELEASE_VERSION}"
   ```

7. Push this new branch to the appropriate remote.  We will open a PR for it **later**, but this will be the **base** for the PR created in the next step).  (And `--set-upstream` will of course track this locally.  This is commonly abbreviated as `-u`.)

   ```
   git push --set-upstream "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "${APOLLO_ROUTER_RELEASE_VERSION}"
   ```

8. Now, open a draft PR with a small boilerplate header from the branch which was just pushed:

   ```
   cat <<EOM | gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create --draft -B "main" --title "release: v${APOLLO_ROUTER_RELEASE_VERSION}" --body-file -
   > **Note**
   > **This particular PR must be true-merged to \`main\`.**

   * This PR is only ready to review when it is marked as "Ready for Review".  It represents the merge to the \`main\` branch of an upcoming release (version number in the title).
   * It will act as a staging branch until we are ready to finalize the release.
   * We may cut any number of alpha and release candidate (RC) versions off this branch prior to formalizing it.
   * This PR is **primarily a merge commit**, so reviewing every individual commit shown below is **not necessary** since those have been reviewed in their own PR.  However, things important to review on this PR **once it's marked "Ready for Review"**:
       - Does this PR target the right branch? (usually, \`main\`)
       - Are the appropriate **version bumps** and **release note edits** in the end of the commit list (or within the last few commits).  In other words, "Did the 'release prep' PR actually land on this branch?"
       - If those things look good, this PR is good to merge!
   EOM
   ```

### Cutting a pre-release

1. Make sure you have all the [Software Requirements](#software-requirements) above fulfilled.

2. Be aware of the version you are cutting a pre-release for.  This would have been picked during the initial [Starting a release PR](#starting-a-release-pr) step.

3. Select a pre-release suffix for the above version.  This could be `-alpha.0`, `-rc.4` or whatever is appropriate.  Most commonly, we cut `-rc.x` releases right before the final release.  Release candidates should have minimal new substantial changes and only changes that are necessary to secure the release.

4. We'll set some environment variables for steps that follow this, which will enable copying-and-pasting subsequent steps.  **Customize these for your own conditions**:

   - Set the version from step 2 as `APOLLO_ROUTER_RELEASE_VERSION`; and
   - Set the pre-release suffix from step 3 as `APOLLO_ROUTER_PRERELEASE_SUFFIX`

     ```
     APOLLO_ROUTER_RELEASE_VERSION="#.#.#"                  # Set me!
     APOLLO_ROUTER_RELEASE_GIT_ORIGIN=origin
     APOLLO_ROUTER_RELEASE_GITHUB_REPO=apollographql/router
     APOLLO_ROUTER_PRERELEASE_SUFFIX="-word.#"              # Set me!
     ```

   After editing, paste the resulting block into your terminal and press _Return_ to activate them.

5. Change your local branch back to the _non-_prep branch, pull any changes you (or others) may have added on GitHub :

    ```
    git checkout "${APOLLO_ROUTER_RELEASE_VERSION}" && \
    git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

6. Run the release automation script using this command to use the environment variable set previously:

   ```
   cargo xtask release prepare --pre-release "${APOLLO_ROUTER_RELEASE_VERSION}${APOLLO_ROUTER_PRERELEASE_SUFFIX}"
   ```

   Running this command will:

     - Bump the necessary versions to the version specified, including those in the documentation.
     - Run our compliance checks and update the `licenses.html` file as appropriate.
     - Ensure we're not using any incompatible licenses in the release.

7. Now, review and stage he changes produced by the previous step.  This is most safely done using the `--patch` (or `-p`) flag to `git add` (`-u` ignores untracked files).

    ```
    git add -up .
    ```

8. Now commit those changes locally, using a brief message:

    ```
    git commit -m "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}${APOLLO_ROUTER_PRERELEASE_SUFFIX}"
    ```

9. Push this commit up to the existing release PR:

    ```
    git push "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

10. Git tag the current commit and & push the branch and the pre-release tag simultaneously:

    This process will kick off the bulk of the release process on CircleCI, including building each architecture on its own infrastructure and notarizing the macOS binary.

    ```
    git tag -a "v${APOLLO_ROUTER_RELEASE_VERSION}${APOLLO_ROUTER_PRERELEASE_SUFFIX}" -m "${APOLLO_ROUTER_RELEASE_VERSION}${APOLLO_ROUTER_PRERELEASE_SUFFIX}" && \
      git push "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "${APOLLO_ROUTER_RELEASE_VERSION}" "v${APOLLO_ROUTER_RELEASE_VERSION}${APOLLO_ROUTER_PRERELEASE_SUFFIX}"
    ```

11. Finally, publish the Crate from your local computer (this also needs to be moved to CI, but requires changing the release containers to be Rust-enabled and to restore the caches):

    > Note: This command may appear unnecessarily specific, but it will help avoid publishing a version to Crates.io that doesn't match what you're currently releasing. (e.g., in the event that you've changed branches in another window) 

    ```
    cargo publish -p apollo-router@"${APOLLO_ROUTER_RELEASE_VERSION}${APOLLO_ROUTER_PRERELEASE_SUFFIX}"
    ```

### Preparing the final release

1. Make sure you have all the [Software Requirements](#software-requirements) above fulfilled.

2. Ensure you have decided the version using [Pick a version](#pick-a-version).

3. We'll set some environment variables for steps that follow this, which will enable copying-and-pasting subsequent steps.  Customize these for your own conditions, **set the version you picked in the above step** as `APOLLO_ROUTER_RELEASE_VERSION`, and then paste into your terminal (press enter to complete it):

   ```
   APOLLO_ROUTER_RELEASE_VERSION="#.#.#"                  # Set me!
   APOLLO_ROUTER_RELEASE_GIT_ORIGIN=origin
   APOLLO_ROUTER_RELEASE_GITHUB_REPO=apollographql/router
   APOLLO_ROUTER_PRERELEASE_SUFFIX=""                     # Intentionally blank.
   ```

4. Change your local branch back to the _non-_prep branch, pull any changes you (or others) may have added on GitHub :

    ```
    git checkout "${APOLLO_ROUTER_RELEASE_VERSION}" && \
    git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}"
    ```

5. Create a new branch called `prep-#.#.#` off of `#.#.#`.  This branch will be used for bumping version numbers and getting final review on the changelog.  We'll do this using the environment variables, so you can just run:

   ```
   git checkout -b "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
   ```

6. On this new `prep-#.#.#` branch, run the release automation script using this command to use the environment variable set previously:

   ```
   cargo xtask release prepare $APOLLO_ROUTER_RELEASE_VERSION
   ```

   Running this command will:

     - Bump the necessary versions to the version specified, including those in the documentation.
     - Migrate the current set of `/.changesets/*.md` files into `/CHANGELOG.md` using the version specified.
     - Run our compliance checks and update the `licenses.html` file as appropriate.
     - Ensure we're not using any incompatible licenses in the release.

7. **MANUALLY CHECK AND UPDATE** the `federation-version-support.mdx` to make sure it shows the version of Federation which is included in the `router-bridge` that ships with this version of Router.  This can be obtained by looking at the version of `router-bridge` in `apollo-router/Cargo.toml` and taking the number after the `+` (e.g., `router-bridge@0.2.0+v2.4.3` means Federation v2.4.3).

11. Now, review and stage he changes produced by the previous step.  This is most safely done using the `--patch` (or `-p`) flag to `git add` (`-u` ignores untracked files).

    ```
    git add -up .
    ```

12. Now commit those changes locally, using a brief message:

    ```
    git commit -m "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

13. _**(Optional)**_ Make local edits to the newly rendered `CHANGELOG.md` entries to do some initial editoral.

    These things should have *ALWAYS* been resolved earlier in the review process of the PRs that introduced the changes, but they must be double checked:

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

18. 🗣️ **Solicit feedback from the Router team on the prep PR**

    Once approved, you can proceed with [Finishing the release](#finishing-the-release).

### Finishing the release

1. Make sure you have all the [Software Requirements](#software-requirements) above fulfilled.

2. Be aware of the version you are finalizing.  This would have been picked during the initial [Starting a release PR](#starting-a-release-pr) step.

3. We'll set some environment variables for steps that follow this, which will enable copying-and-pasting subsequent steps.  Customize these for your own conditions, **set the version you picked in the above step** as `APOLLO_ROUTER_RELEASE_VERSION`, and then paste into your terminal (press enter to complete it):

   ```
   APOLLO_ROUTER_RELEASE_VERSION="#.#.#"                  # Set me!
   APOLLO_ROUTER_RELEASE_GIT_ORIGIN=origin
   APOLLO_ROUTER_RELEASE_GITHUB_REPO=apollographql/router
   APOLLO_ROUTER_PRERELEASE_SUFFIX=""                     # Intentionally blank.
   ```

4. Use the `gh` CLI to **squash** (**_NOT true-merge_**) on the prep PR opened previously:

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --squash --body "" -t "prep release: v${APOLLO_ROUTER_RELEASE_VERSION}" "prep-${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

5. After the prep PR has squash-merged into the release PR, change your local branch back to release branch, pull any changes you (or others) may have added on GitHub, so you have them locally:

    ```
    git checkout "${APOLLO_ROUTER_RELEASE_VERSION}" && \
    git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

6. Use the `gh` CLI to enable **auto-merge** (**_NOT_** auto-**_squash_**):

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge --merge --body "" -t "release: v${APOLLO_ROUTER_RELEASE_VERSION}" --auto "${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

7. 🗣️ **Solicit approval from the Router team, wait for the PR to pass CI and auto-merge into `main`**

8. After the PR has merged to `main`, pull `main` to your local terminal, and Git tag & push the release:

    This process will kick off the bulk of the release process on CircleCI, including building each architecture on its own infrastructure and notarizing the macOS binary.

    ```
    git checkout main && \
    git pull "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" && \
    git tag -a "v${APOLLO_ROUTER_RELEASE_VERSION}" -m "${APOLLO_ROUTER_RELEASE_VERSION}" && \
    git push "${APOLLO_ROUTER_RELEASE_GIT_ORIGIN}" "v${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

9. Open a PR that reconciles `dev` (Make sure to merge this reconciliation PR back to dev, **do not squash or rebase**):

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr create --title "Reconcile \`dev\` after merge to \`main\` for v${APOLLO_ROUTER_RELEASE_VERSION}" -B dev -H main --body "Follow-up to the v${APOLLO_ROUTER_RELEASE_VERSION} being officially released, bringing version bumps and changelog updates into the \`dev\` branch."
    ```

10. Mark the PR to **auto-merge NOT auto-squash** using the URL that is output from the previous command

    ```
    APOLLO_RECONCILE_PR_URL=$(gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr list --state open --base dev --head main --json url --jq '.[-1] | .url')
    test -n "${APOLLO_RECONCILE_PR_URL}" && \
      gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" pr merge "${APOLLO_RECONCILE_PR_URL}"
    ```


11. 🗣️ **Solicit approval from the Router team, wait for the PR to pass CI and auto-merge into `dev`**

12. 👀 Follow along with the process by [going to CircleCI for the repository](https://app.circleci.com/pipelines/github/apollographql/router) and clicking on `release` for the Git tag that appears at the top of the list.

13. ⚠️ **Wait for `publish_github_release` on CircleCI to finish on this job before continuing.** ⚠️

    You should expect this will take at least 30 minutes.

14. Re-create the file you may have previously created called `this_release.md` just to make sure its up to date after final edits from review:

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

15. Change the links in `this_release.md` from `[@username](https://github.com/username)` to `@username` in order to facilitate the correct "Contributorship" attribution on the final GitHub release.

    ```
    perl -pi -e 's/\[@([^\]]+)\]\([^)]+\)/@\1/g' this_release.md
    ```

16. Update the release notes on the now-published [GitHub Releases](https://github.com/apollographql/router/releases) (this needs to be moved to CI, but requires `this_release.md` which we just created):

    ```
    gh --repo "${APOLLO_ROUTER_RELEASE_GITHUB_REPO}" release edit v"${APOLLO_ROUTER_RELEASE_VERSION}" -F ./this_release.md
    ```

17. Finally, publish the Crate from your local computer from the `main` branch (this also needs to be moved to CI, but requires changing the release containers to be Rust-enabled and to restore the caches):

    > Note: This command may appear unnecessarily specific, but it will help avoid publishing a version to Crates.io that doesn't match what you're currently releasing. (e.g., in the event that you've changed branches in another window) 

    ```
    cargo publish -p apollo-router@"${APOLLO_ROUTER_RELEASE_VERSION}"
    ```

18. (Optional) To have a "social banner" for this release, run [this `htmlq` command](https://crates.io/crates/htmlq) (`cargo install htmlq`, or on MacOS `brew install htmlq`; its `jq` for HTML), open the link it produces, copy the image to your clipboard:

    ```
    curl -s "https://github.com/apollographql/router/releases/tag/v${APOLLO_ROUTER_RELEASE_VERSION}" | htmlq 'meta[property="og:image"]' --attribute content
    ```

## Nightly Releases

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

## Troubleshooting a release

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
