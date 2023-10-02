[![Version](https://img.shields.io/crates/v/apollo-router.svg)](https://crates.io/crates/apollo-router)
[![Docs.rs](https://docs.rs/apollo-router/badge.svg)](https://docs.rs/apollo-router)

# Apollo Router

[<img alt="Apollo Router" src="https://raw.githubusercontent.com/apollographql/space-kit/main/src/illustrations/svgs/satellite1.svg" height="144">](https://www.apollographql.com/docs/router/)

The **Apollo Router** is a configurable, high-performance **graph router** written in Rust to run a [federated supergraph](https://www.apollographql.com/docs/federation/) that uses [Apollo Federation 2](https://www.apollographql.com/docs/federation/v2/federation-2/new-in-federation-2). It is well-tested, regularly benchmarked, includes major features of Apollo Gateway and serves production-scale workloads.

The latest release can always be found at the top of [the Releases page](https://github.com/apollographql/router/releases/) along with past releases and release notes.  We follow the [Semantic Versioning 2.0.0](https://semver.org/) specification when publishing new versions.  A [`CHANGELOG.md` is also included](https://github.com/apollographql/router/blob/main/CHANGELOG.md) in the Git repository with information about each release.

## Getting started

Follow the [quickstart tutorial](https://www.apollographql.com/docs/router/quickstart/) to get up and running with the Apollo Router.

See [the documentation](https://www.apollographql.com/docs/router) for more details and guides on running the Router:

- using Helm charts in Kubernetes
- with pre-published Docker images
- including additional customizations
- from source, and more!

## Using the Apollo Router as a library

Most Apollo Router features can be defined using our [YAML configuration](https://www.apollographql.com/docs/router/configuration/overview) and many customizations can be written with [Rhai scripts](https://www.apollographql.com/docs/router/customizations/rhai) which work on published binaries of the Router and do not require compilation.

If you prefer to write customizations in Rust or need more advanced customizations, see our section on [native customizations](https://www.apollographql.com/docs/router/customizations/native) for information on how to use `apollo-router` as a Rust library.  We also publish Rust-specific documentation on our [`apollo-router` crate docs](https://docs.rs/crate/apollo-router).

<!-- renovate-automation: rustc version -->
The minimum supported Rust version (MSRV) for this version of `apollo-router` is **1.72.0**.
