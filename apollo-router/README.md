[![Version](https://img.shields.io/crates/v/apollo-router.svg)](https://crates.io/crates/apollo-router)
[![Docs.rs](https://docs.rs/apollo-router/badge.svg)](https://docs.rs/apollo-router)

# Apollo Router

[<img alt="Apollo Router" src="https://raw.githubusercontent.com/apollographql/space-kit/main/src/illustrations/svgs/satellite1.svg" height="144">](https://www.apollographql.com/docs/router/)

The **Apollo Router** is a configurable, high-performance **graph router** written in Rust to run a [federated supergraph](https://www.apollographql.com/docs/federation/) that uses [Apollo Federation 2](https://www.apollographql.com/docs/federation/v2/federation-2/new-in-federation-2).

Apollo Router is well-tested, regularly benchmarked, includes most major features of Apollo Gateway and is able to serve production-scale workloads.  Please note that the (pre-1.0) version is not yet "semver stable" and we may still make breaking changes.  Generally speaking, we expect most breaking changes to be on the plugin API and the configuration file format.  We will clearly convey such changes in the release notes.

New releases and their release notes (along with notes about any breaking changes) can be found on the [Releases](https://github.com/apollographql/router/releases) page, and the latest release can always be found [on the latest page](https://github.com/apollographql/router/releases/latest).

## Getting started

Follow the [quickstart tutorial](https://www.apollographql.com/docs/router/quickstart/) to get up and running with the Apollo Router.

See [the documentation](https://www.apollographql.com/docs/router) for more details.

## Using the Apollo Router as a library

See our section on [native customizations](https://www.apollographql.com/docs/router/customizations/native) for information on how to use `apollo-router` as a library.
