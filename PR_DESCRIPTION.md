# Add router config schema to release artifacts (without extra build latency)

## Summary

Re-introduces config schema generation for GitHub releases in a way that **does not add** the ~18 minute Rust build that caused [PR #8609](https://github.com/apollographql/router/pull/8609) to be reverted in [#8896](https://github.com/apollographql/router/pull/8896).

Instead of building the router again in `publish_github_release`, the schema is generated in the **amd_linux_build** job using the binary already produced by `cargo xtask dist` in that same job, then persisted to the workspace so it is included in the release.

## Changes

1. **Generate config schema in `build_release` (amd_linux only)**  
   After `cargo xtask package` in the amd_linux_build branch, a new step runs the already-built `./target/release/router config schema` and writes a **versioned** file into `artifacts/`:
   - Version is taken from Cargo metadata (same as tarball names).
   - Filename: `router-config-schema-{version}.json` (e.g. `router-config-schema-v2.12.0.json`).
   - Release URL example: `https://github.com/apollographql/router/releases/download/v2.12.0/router-config-schema-v2.12.0.json`.

2. **Remove the build + schema step from `publish_github_release`**  
   The step that ran `cargo build --release --bin router` and then `router config schema > artifacts/config-schema.json` has been removed. The publish job now only attaches the workspace (which already contains the versioned schema from amd_linux_build) and continues with checksums, Crates.io, GitHub release, Docker, and Helm.

## Why amd_linux_build?

- **publish_github_release** runs on the same platform (Linux x86_64) as amd_linux_build, so the only job that can run the router binary without a new build is amd_linux_build.
- macos_build / windows_build produce binaries for other OSes; arm_linux_build produces a Linux ARM binary, not usable on the publish executor.
- Only one job writes the schema file to avoid duplicate paths when CircleCI merges workspaces.

## Nightly

The same amd_linux_build step runs for nightly, so nightly artifacts (e.g. from the amd_linux job’s stored artifacts) also include the versioned schema file with no extra build.

---

**After opening this PR:** Update `.changesets/feat_config_schema_from_build_release.md` and replace `.../pull/0` with your actual PR URL (e.g. `.../pull/9XXX`).
