# xtask

The Apollo Router project uses [xtask](https://github.com/matklad/cargo-xtask) to help with the automation of code quality. 

You can run `cargo xtask --help` to see the usage. Generally we recommend that you continue to use the default cargo commands like `cargo fmt`, `cargo clippy`, and `cargo test`, but if you are debugging something that is happening in CI it can be useful to run the xtask commands that we run [in CI](../.github/workflows).

## xtask dev

`xtask dev` runs all the checks, linting, and tests that we also run on CI. Run it locally before creating a PR to check your work.

## xtask lint

`xtask lint` runs code formatting checks and clippy. Use `cargo xtask lint --fmt` to fix formatting issues.

## xtask test

You can run `cargo xtask test` to run tests with the same configuration as our CI systems. If you are on GNU Linux, it will also run the e2e tests set up in [apollographql/supergraph-demo](https://github.com/apollographql/supergraph-demo).

## xtask dist

You can run `cargo xtask dist` to build the Apollo Router's binary like it would be built in CI. It will automatically build from the source that you've checked out and for your local machine's architecture. If you would like to build a specific version of Router, you can pass `--version v0.1.5` where `v0.1.5` is the version you'd like to build.

## xtask release

This command group prepares the Apollo Router for a new release. A step-by-step guide on doing so is in the [release checklist](../RELEASE_CHECKLIST.md).

## xtask fed-flame

`cargo xtask fed-flame` is a helper for producing flame graphs for query planning. This is useful to investigate the performance of a query that you know is slow to plan. Typical usage:

```
cargo xtask fed-flame plan query.graphql schema.graphql
```

For query planner developers, some more guidance on using flame graphs is available in the Federation Confluence space.
