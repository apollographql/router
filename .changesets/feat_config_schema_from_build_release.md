### Add versioned router config schema to release artifacts without extra build

The router configuration schema JSON is again included in GitHub releases, but generated **without** adding a full Rust build to the release pipeline. (A previous approach in [PR #8609](https://github.com/apollographql/router/pull/8609) was reverted in [#8896](https://github.com/apollographql/router/pull/8896) because it added ~18 minutes to the release.)

The schema is now produced in the **amd_linux_build** job using the binary already built by `cargo xtask dist` in that job. The output is written to a versioned file (e.g. `router-config-schema-v2.12.0.json`) so each release has a clearly versioned schema. The file is persisted to the workspace and included in the GitHub release alongside the other artifacts. Nightly builds also produce the versioned schema in the amd_linux artifacts with no extra build.

By [@shanemyrick](https://github.com/shanemyrick)
