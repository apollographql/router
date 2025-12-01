# Development

The **Apollo Router Core** is a configurable, high-performance **graph router** for a [federated graph](https://www.apollographql.com/docs/federation/):

## Crates

* `apollo-federation` - federated graph composition and query planning
* `apollo-router` - the Apollo Router server

## Development

We use [`mise`](https://mise.jdx.dev/) to manage toolchain and test dependencies.
Example commands in this document assume that mise shims are available in the environment.
If you do not use `mise activate`, you can use `mise x -- <COMMAND>` in place of the commands below, for example `mise x -- cargo build`.

### Getting started

Use `cargo build --all-targets` to build the project.

#### External test dependencies

Some tests require external services for caching, telemetry, and database functionality.

To start these services:

```shell
docker compose up -d
```

This starts:
- **Redis** (port 6379, 7000-7005) - Required for entity caching, response caching, and Redis-related integration tests
- **Zipkin** (port 9411) - For distributed tracing tests
- **Datadog Agent** (port 8126) - For Datadog telemetry integration tests

Some tests that use the features above are configured with `required_to_start: true`. The router won't start if these services aren't available, causing test failures.

**Note:** `-d` runs services in the background. Remove `-d` if you want to see logs or run in foreground.

Several tests also require the `redis-cli` binary; this is installed by default if you use `mise`.

#### Enterprise feature testing

Some tests require Apollo GraphOS credentials to test enterprise features like licensing, reporting, and Apollo Studio integration.

If you have access to a GraphOS graph, set these environment variables:

```shell
export TEST_APOLLO_KEY="your-apollo-api-key"
export TEST_APOLLO_GRAPH_REF="your-graph-ref@variant"
```

**When these are NOT set:** Enterprise tests will be automatically skipped rather than failing. This is gated by a `graph_os_enabled` function used in tests. _Developers: to ensure that enterprise tests are skipped, make sure to include this check!_
**When these ARE set:** Tests will connect to Apollo GraphOS services for full integration testing.

### Testing

Tests on this repository are run using [nextest](https://nexte.st/). nextest is installed automatically when you use
`mise`.

#### Test environment setup

**For basic unit and integration tests:**
```shell
# Start external services (eg, Redis, PostgreSQL)
docker compose up -d
```

**For enterprise/GraphOS feature tests:**

This is optional. See above for how these tests will be skipped when these environment variables aren't set along with other nuances of how tests are run.

```shell
# Set GraphOS credentials (optional)
export TEST_APOLLO_KEY="your-apollo-api-key"
export TEST_APOLLO_GRAPH_REF="your-graph-ref@variant"
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

For more complex test selection, nextest supports [filtersets](https://nexte.st/docs/filtersets/reference/) using the `-E` flag, which allow you to run specific subsets of tests using logical operators and pattern matching.

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
