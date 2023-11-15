# Development

The **Apollo Router** is a configurable, high-performance **graph router** for a [federated graph](https://www.apollographql.com/docs/federation/):

## Crates

 *   `configuration` - Config model and loading.
 *   `query planner` - Query plan model and a caching wrapper for calling out to the nodejs query planner.
 *   `execution` - Converts a query plan to a stream.
 *   `server` - Handles requests,
     obtains a query plan from the query planner,
     obtains an execution pipeline,
     returns the results

## Binaries

 *   `router` - Starts a server.

## Development

You will need a recent version of rust (`1.72` works well as of writing). 
Installing rust [using rustup](https://www.rust-lang.org/tools/install) is
the recommended way to do it as it will install rustup, rustfmt and other 
goodies that are not always included by default in other rust distribution channels:

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

In addition, you will need to [install protoc](https://grpc.io/docs/protoc-installation/) and [cmake](https://cmake.org/).

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

### Run Apollo Router against the docker-compose or Node.js setup

Once the subgraphs are up and running, run Apollo Router with this command:

```shell
cargo run --release -- -s ./examples/graphql/local.graphql -c examples/telemetry/jaeger.router.yaml
```

Go to https://studio.apollographql.com/sandbox/explorer to make queries and
http://localhost:16686/ to reach Jaeger.

### Strict linting and license compliance

While developing locally doc warnings and other lint checks are disabled.
This limits the noise generated while exploration is taking place.

When you are ready to create a PR, run a build with strict checking enabled,
and check for license compliance.

Use `cargo xtask all` to run all of the checks the CI will run.

The CI checks require `cargo-deny` and `cargo-about` which can both be installed by running:
- `cargo install cargo-deny`
- `cargo install cargo-about`

Updating the snapshots used during testing requires installing `cargo-insta`:
- `cargo install cargo-insta`

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
# e.g. heap and ad-hoc allocation tracing: cargo build --profile release-dhat --features dhat-heap,dhat-ad-hoc
```

e.g.: heap allocation tracing
```shell
cargo build --profile release-dhat --features dhat-heap 
```

This will create a router in `./target/release-dhat`.

When you run your binary, on termination you will get `dhat-heap.json` and/or `dhat-ad-hoc.json` files which can
be examined using standard DHAT tooling.

For more details on interpreting these files and running tests, see the [dhat-rs](https://docs.rs/dhat/latest/dhat/#running) crate documentation.

### Troubleshoot

+ If you have an issue with rust-analyzer reporting an unresolved import about `derivative::Derivative` [check this solution](https://github.com/rust-analyzer/rust-analyzer/issues/7459#issuecomment-876796459) found in a rust-analyzer issue.

## Project maintainers

Apollo Graph, Inc.
