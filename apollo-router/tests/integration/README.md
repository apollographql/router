# Apollo Router Integration Tests

This directory contains integration tests for the Apollo Router. These tests verify the router's behavior in realistic scenarios by starting actual router processes and testing their functionality.

## Prerequisites

Before running integration tests, ensure you have the required external services:

```shell
# From the repository root
docker-compose up -d
```

**For enterprise feature tests** (optional):
```shell
export TEST_APOLLO_KEY="your-apollo-api-key"  
export TEST_APOLLO_GRAPH_REF="your-graph-ref@variant"
```

Without these environment variables, enterprise tests will be skipped. See notes in the [development README](../../DEVELOPMENT.md#testing) for nuances of how tests are run.

## Test Categories

**Standard tests:** Work with just Docker Compose services (most will fail if services aren't running)
**Enterprise tests:** Require GraphOS credentials (entity caching, Apollo reporting, etc.)  
**CI-only tests:** Some Redis/PostgreSQL tests only run on Linux x86_64 in CI

**Note:** Most integration tests are configured with `required_to_start: true` for Redis/PostgreSQL, meaning they will fail at startup if these services aren't available. A few tests use `required_to_start: false` and will continue without the services.

## Using `xtask`

See the [xtask README](../../xtask/README.md) for the commands that are useful for checks, linting, and running the tests. Using `xtask` is an easy way to run the integration tests!

## Using `cargo nextest`

We use [nextest](https://nexte.st/) as the recommended test runner. See the [development README](../../DEVELOPMENT.md#testing) for details, but nextest provides faster and more reliable test execution! There are more details there on how to install `nextest` and use it to target all integration tests or individual tests, along with using `RUST_LOG`. See the section below on log-levels to understand why you might want to use different levels while testing (especially if you encounter flaky tests locally).

## Using `cargo` to run the integration tests

### Running All Integration Tests

Running all of the integration tests is fairly straightforward:

```shell
cargo test --test integration_tests
```


### Running Individual Integration Tests

Sometimes, though, you might want to work on a specific integration test. You can run specific test modules or individual tests like this:

```shell
# Run all lifecycle module tests
cargo test --test integration_tests integration::lifecycle

# Run a specific test (e.g., test_happy)
cargo test --test integration_tests integration::lifecycle::test_happy
```

## Log-level configuration

Integration tests often require examining log output for debugging. The `RUST_LOG` environment variable controls logging verbosity.

### Common Log Level Examples

```shell
# Basic info-level logging
RUST_LOG=info cargo test --test integration_tests integration::lifecycle::test_happy

# Full debug logging (very verbose)
RUST_LOG=debug cargo test --test integration_tests integration::lifecycle::test_happy
```

### Reducing Log Noise

Some third-party libraries produce excessive debug output that can obscure useful information or take a significant amount of processing time. This can be a problem! We match on a magic string to denote a healthy router coming online, and if a dependency is producing more logs than we can process before timing out that check, tests will fail. 

So, if you're experiencing flaky tests while using debug-level logging, excessive logs might be the culprit. Find the noisiest dependency and tune its level:

```shell
# Reduce verbose jsonpath_lib logging while keeping debug for everything else
RUST_LOG=debug,jsonpath_lib=info cargo test --test integration_tests integration::lifecycle::test_happy

# Multiple library filters
RUST_LOG=trace,jsonpath_lib=info,hyper=debug cargo test --test integration_tests integration::lifecycle::test_happy
```
