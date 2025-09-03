# Development

The **Apollo Router Core** is a configurable, high-performance **graph router** for a [federated graph](https://www.apollographql.com/docs/federation/):

## Crates

* `configuration` - Config model and loading.
* `query planner` - Query plan model and a caching wrapper for calling out to the nodejs query planner.
* `execution` - Converts a query plan to a stream.
* `server` - Handles requests,
     obtains a query plan from the query planner,
     obtains an execution pipeline,
     returns the results

## Binaries

* `router` - Starts a server.

## Development

You will need a recent version of rust, as specified in `rust-toolchain.toml`.
We recommend [using rustup](https://www.rust-lang.org/tools/install)
as it will automatically install the requiried toolchain version,
including rustfmt and clippy
that are not always included by default in other rust distribution channels.

In addition, you will need to [install protoc](https://grpc.io/docs/protoc-installation/).

Set up your git hooks:

```shell
git config --local core.hooksPath .githooks/
```

### Getting started

Use `cargo build --all-targets` to build the project.

Some tests use external services such as Jaeger and Redis.

To start these services:

```
docker-compose up -d
```

**Note:** `-d` is for running into background. You can remove `-d` if you
have issues and you want to see the logs or if you want to run the service
in foreground.

### Testing

Tests on this repository are run using [nextest](https://nexte.st/).

#### Installing nextest

If you don't already have nextest installed:

```shell
cargo install cargo-nextest --locked
```

#### Using nextest with integration tests

```shell
# Run all integration tests
cargo nextest run --test integration_tests

# Run all lifecycle module tests
cargo nextest run --test integration_tests -E 'test(integration::lifecycle)'

# Run a specific test (e.g., test_happy)
cargo nextest run --test integration_tests -E 'test(integration::lifecycle::test_happy)'
```

#### Using nextest for unit tests, with filters

```shell
# Run a single unit test
cargo nextest run --lib -E 'test(test_router_trace_attributes)'
# Run a suite of unit tests
cargo nextest run --lib -p apollo-router -E 'test(services::router)'
```

### Run against the docker-compose or Node.js setup

Once the subgraphs are up and running, run the router with this command:

```shell
cargo run --release -- -s ./examples/graphql/local.graphql -c examples/telemetry/jaeger.router.yaml
```

Go to <https://studio.apollographql.com/sandbox/explorer> to make queries and
<http://localhost:16686/> to reach Jaeger.

### Strict linting and license compliance

While developing locally doc warnings and other lint checks are disabled.
This limits the noise generated while exploration is taking place.

When you are ready to create a PR, run a build with strict checking enabled,
and check for license compliance.

Use `cargo xtask all` to run all of the checks the CI will run.

The CI checks require `cargo-deny` and `cargo-about` which can both be installed by running:

* `cargo install cargo-deny`
* `cargo install cargo-about`

Updating the snapshots used during testing requires installing `cargo-insta`:

* `cargo install cargo-insta`

They also need you to have the federation-demo project up and running,
as explained in the Getting started section above.

### Yaml configuration design

If you are adding a new feature or modifying an existing feature then consult the [yaml design guidance](dev-docs/yaml-design-guidance.md) page.

### Investigating memory usage

There are two features: `dhat-heap` and `dhat-ad-hoc` which may be enabled for investigating memory issues
with the router. You may enable either or both, depending on the kind of problem you are investigating.

You have to build the router with your choice of feature flags and you must use the `release-dhat` profile.

e.g.: heap and ad-hoc allocation tracing

```shell
cargo build --profile release-dhat --features dhat-heap,dhat-ad-hoc
```

e.g.: heap allocation tracing

```shell
cargo build --profile release-dhat --features dhat-heap
```

This will create a router in `./target/release-dhat`, which can be run with:
```shell
cargo run --profile release-dhat --features dhat-heap -- -s ./apollo-router/testing_schema.graphql -c router.yaml
```

When you run your binary, on termination you will get `dhat-heap.json` and/or `dhat-ad-hoc.json` files which can
be examined using standard DHAT tooling, e.g. [DHAT html viewer](https://nnethercote.github.io/dh_view/dh_view.html)

For more details on interpreting these files and running tests, see the [dhat-rs](https://docs.rs/dhat/latest/dhat/#running) crate documentation.

### Troubleshoot

* If you have an issue with rust-analyzer reporting an unresolved import about `derivative::Derivative` [check this solution](https://github.com/rust-analyzer/rust-analyzer/issues/7459#issuecomment-876796459) found in a rust-analyzer issue.

### Code coverage

Code coverage is run in CI nightly, but not done on every commit.  To view coverage from nightly runs visit [our coverage on Codecov](https://codecov.io/gh/apollographql/router).

To run code coverage locally, you can `cargo install cargo-llvm-cov`, and run:

```shell
cargo llvm-cov nextest --summary-only
```

For full information on available options, including HTML reports and `lcov.info` file support, see [nextest documentation](https://nexte.st/book/coverage.html) and [cargo llvm-cov documentation](https://github.com/taiki-e/cargo-llvm-cov#get-coverage-of-cc-code-linked-to-rust-librarybinary).

## Project maintainers

Apollo Graph, Inc.
