# xtask

The Apollo Router project uses [xtask](https://github.com/matklad/cargo-xtask) to help with the automation of code quality. 

You can run `cargo xtask --help` to see the usage. Generally we recommend that you continue to use the default cargo commands like `cargo fmt`, `cargo clippy`, and `cargo test`, but if you are debugging something that is happening in CI it can be useful to run the xtask commands that we run [in CI](../.github/workflows).

## xtask test

You can run `cargo xtask test` to run tests with the same configuration as our CI systems. If you are on GNU Linux, it will also run the e2e tests set up in [apollographql/supergraph-demo](https://github.com/apollographql/supergraph-demo).

## xtask dist

You can run `cargo xtask dist` to build the Apollo Router's binary like it would be built in CI. It will automatically build from the source that you've checked out and for your local machine's architecture. If you would like to build a specific version of Router, you can pass `--version v0.1.5` where `v0.1.5` is the version you'd like to build.

## xtask prep

The most important xtask command you'll need to run locally is `cargo xtask prep`. This command prepares the Apollo Router for a new release. You'll want to update the version in `Cargo.toml`, and run `cargo xtask prep` as a part of making the PR for a new release. 

These steps are outlined in more detail in the [release checklist](../RELEASE_CHECKLIST.md).
