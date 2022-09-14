# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

> **Note**
> We are entering our release candidate ("RC") stage and expect this to be the last of our breaking changes.  Overall, most of the breaking changes in this release revolve around three key factors which were motivators for most of the changes:
>
> 1. Having **safe and security defaults** which are suitable for production
> 2. Polishing our YAML configuration ergonomics and patterns
> 3. The introduction of a development mode activated with the `--dev` flag
>
> See the full changelog below for details on these (including the "Features" section for the `--dev` changes!)

### Adjusted socket ("listener") addresses for more secure default behaviors

- The Router will not listen on "all interfaces" in its default configuration (i.e., by binding to `0.0.0.0`).  You may specify a specific socket by specifying the `interface:port` combination.  If you desire behavior which binds to all interfaces, your configuration can specify a socket of `0.0.0.0:4000` (for port `4000` on all interfaces).
- By default, Prometheus (if enabled) no longer listens on the same socket as the GraphQL socket.  You can change this behavior by binding it to the same socket as your GraphQL socket in your configuration.
- The health check endpoint is no longer available on the same socket as the GraphQL endpoint (In fact, the health check suggestion has changed in ways that are described elsewhere in this release's notes.  Please review them separately!)

### Safer out-of-the box defaults with `sandbox` and `introspection` disabled ([PR #1748](https://github.com/apollographql/router/pull/1748))

To reflect the fact that it is not recomended to have introspection on in production (and since Sandbox uses introspection to power its development features) the `sandbox` and `introspection` configuration are now **disabled unless you are running the Router with `--dev`**.

If you would like to force them on even when outside of `--dev` mode, you can set them to `true` explicitly in your YAML configuration:

```yaml
sandbox:
  enabled: true
supergraph:
  introspection: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1748

### Landing page ("home page") replaces Sandbox in default "production" mode ([PR #1768](https://github.com/apollographql/router/pull/1768))

As an extension of Sandbox and Introspection being disabled by default (see above), the Router now displays a simple landing page when running in its default mode.  When you run the Apollo Router with the new `--dev` flag (see "Features" section below) you will still see the existing "Apollo Studio Sandbox" experience.

We will offer additional options to customize the landing page in the future but for now you can disable the homepage entirely (leaving a _very_ generic page with a GraphQL message) by disabling the homepage entirely in your configuration:

```yaml
homepage:
  enabled: false
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1768

### Listeners, paths and paths can be configured individually  ([Issue #1500](https://github.com/apollographql/router/issues/1500))

It is now possible to individually configure the following features' socket/listener addresses (i.e., the IP address and port) in addition to the URL path:

- GraphQL execution (default: `http://127.0.0.1:4000/`)
- Sandbox (default when using `--dev`: `http://127.0.0.1:4000/`)
- Prometheus (default when enabled: `http://127.0.0.1:9090/metrics`)

Examples of how to configure these can be seen in the YAML configuration overhaul section of this changelog (just below) as well as in our documentation.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1718

### Overhaul/reorganization of YAML configuration ([#1500](https://github.com/apollographql/router/issues/1500))

To facilitate the changes in the previous bullet-points, we have moved configuration parameters which previously lived in the `server` section to new homes in the configuration, including `listen`, `graphql_path`, `landing_page`, and `introspection`.  Additionally, `preview_defer_support` has moved, but is on by default and no longer necessary to be set explicitly unless you wish to disable it.

As another section (below) notes, we have *removed* the health check and instead recommend users to configure their health checks (in, e.g, Kubernetes, Docker, etc.) to use a simple GraphQL query: `/?query={__typename}`.  Read more about that in the other section, however this is reflected by its removal in the configuration.

To exemplify the changes, this previous configuration will turn into the configuration that follows it:

#### Before

```yaml
server:
  listen: 127.0.0.1:4000
  graphql_path: /graphql
  health_check_path: /health # Health check has been deprecated.  See below.
  introspection: false
  preview_defer_support: true
  landing_page: true
telemetry:
  metrics:
    prometheus:
      enabled: true
```

#### After

```yaml
# This section is just for Sandbox configuration
sandbox:
  listen: 127.0.0.1:4000
  path: /
  enabled: false # Disabled by default, but on with `--dev`.

# This section represents general supergraph GraphQL execution
supergraph:
  listen: 127.0.0.1:4000
  path: /
  introspection: false
  # Can be removed unless it needs to be set to `false`.
  preview_defer_support: true

# The health check has been removed.  See the section below in the CHANGELOG
# for more information on how to configure health checks going forward.

# Prometheus scraper endpoint configuration
# The `listen` and `path` are not necessary if `127.0.0.1:9090/metrics` is okay
telemetry:
  metrics:
    prometheus:
      listen: 127.0.0.1:9090
      path: /metrics
      enabled: true
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1718

### Environment variable expansion adjustments ([#1759](https://github.com/apollographql/router/issues/1759))

- Environment expansions **must** be prefixed with `env.`.
- File expansions **must** be prefixed with `file.`.
- The "default" designator token changes from `:` to `:-`. For example:

  `${env.USER_NAME:Nandor}` => `${env.USER_NAME:-Nandor}`

- Failed expansions now result in an error

  Previously expansions that failed due to missing environment variables were silently skipped. Now they result in a configuration error. Add a default value using the above syntax if optional expansion is needed.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1763

### Dedicated health check endpoint removed with new recommendation to use `/query={__typename}` query ([Issue #1765](https://github.com/apollographql/router/issues/1765))

We have *removed* the dedicated health check endpoint and now recommend users to configure their health checks (in, e.g, Kubernetes, Docker) to use a simple GraphQL query instead.

Use the following query with a `content-type: application/json` header as a health check instead of `/.well-known/apollo/server-health`:

```
/?query={__typename}
```

The [Kubernetes documentation and related Helm charts](https://www.apollographql.com/docs/router/containerization/kubernetes) have been updated to reflect this change.

Using this query has the added benefit of *actually testing GraphQL*.  If this query returns with an HTTP 200 OK, it is just as reliable (and even more meaningful) than the previous `/.well-known/apollo/server-health` endpoint.  It's important to include the `content-type: application/json` header to satisfy the Router's secure requirements that offer CSRF protections.

In the future, we will likely reintroduce a dedicated health check "liveliness" endpoint along with a meaningful "readiness" health check at the same time.  In the meantime, the query above is technically more durable than the health check we offered previously.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/TODO

### Promote `include_subgraph_errors` out of "experimental" status ([Issue #1773](https://github.com/apollographql/router/issues/1773))

The `include_subraph_errors` plugin has been promoted out of "experimental" and will require a small configuration changes.  For example:

```diff
-plugins:
-  experimental.include_subgraph_errors:
-    all: true # Propagate errors from all subraphs
-    subgraphs:
-      products: false # Do not propagate errors from the products subgraph
+include_subgraph_errors:
+  all: true # Propagate errors from all subraphs
+  subgraphs:
+    products: false # Do not propagate errors from the products subgraph
 ```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1776

### `apollo-spaceport` and `uplink` are now part of `apollo-router` ([Issue #491](https://github.com/apollographql/router/issues/491))

Instead of being dependencies, they are now part of the `apollo-router` crate.  They were not meant to be used independently.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1751

### Remove over-exposed functions from the public API ([PR #1746](https://github.com/apollographql/router/pull/1746))

The following functions are only required for router implementation, so removing from external API:

```
subgraph::new_from_response
supergraph::new_from_response
supergraph::new_from_graphql_response
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1746

### Span `client_name` and `client_version` attributes renamed ([#1514](https://github.com/apollographql/router/issues/1514))

OpenTelemetry attributes should be grouped by `.` rather than `_`, therefore the following attributes have changed:

* `client_name` => `client.name`
* `client_version` => `client.version`

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1514

### Otel configuration updated to use expansion ([#1772](https://github.com/apollographql/router/issues/1772))

File and env access in configuration now use the generic expansion mechanism introduced in [#1759](https://github.com/apollographql/router/issues/1759).

```yaml
      grpc:
        key:
          file: "foo.txt"
        ca:
          file: "bar.txt"
        cert:
          file: "baz.txt"
```

Becomes:
```yaml
      grpc:
        key: "${file.foo.txt}"
        ca: "${file.bar.txt}"
        cert: "${file.baz.txt}"
```
or
```yaml
      grpc:
        key: "${env.FOO}"
        ca: "${env.BAR}"
        cert: "${env.BAZ}"
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1774

## 🚀 Features

### Adds a development mode that can be enabled with the `--dev` flag ([#1474](https://github.com/apollographql/router/issues/1474))

By default, the Apollo Router is configured with production best-practices.  When developing, it is often desired to have some of those features relaxed to make it easier to iterate.  A `--dev` flag has been introduced to make the user experience easier while maintaining a default configuration which targets a productionized environment.

The `--dev` mode will enable a few options _for development_ which are not normally on by default:

- The Apollo Sandbox Explorer will be served instead of the Apollo Router landing page, allowing you to run queries against your development Router.
- Introspection will be enabled, allowing client tooling (and Sandbox!) to obtain the latest version of the schema.
- Hot-reloading of configuration will be enabled. (Also available with `--hot-reload` when running without `--dev`)
- It will be possible for Apollo Sandbox Explorer to request a query plan to be returned with any operations it executes. These query plans will allow you to observe how the operation will be executed against the underlying subgraphs.
- Errors received from subgraphs will not have their contents redacted to facilitate debugging.

Additional considerations will be made in the future as we introduce new features that might necessitate a "development" workflow which is different than the default mode of operation.  We will try to minimize these differences to avoid surprises in a production deployment while providing an execellent development experience.  In the future, the (upcoming) `rover dev` experience will become our suggested pattern, but this should serve the purpose in the near term.

By [@bnjjj](https://github.com/bnjjj) and [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) and [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1748

### Apollo Studio Federated Tracing ([#1514](https://github.com/apollographql/router/issues/1514))

Add support of [federated tracing](https://www.apollographql.com/docs/federation/metrics/) in Apollo Studio:

```yaml
telemetry:
    apollo:
        # The percentage of requests will include HTTP request and response headers in traces sent to Apollo Studio.
        # This is expensive and should be left at a low value.
        # This cannot be higher than tracing->trace_config->sampler
        field_level_instrumentation_sampler: 0.01 # (default)

        # Include HTTP request and response headers in traces sent to Apollo Studio
        send_headers: # other possible values are all, only (with an array), except (with an array), none (by default)
            except: # Send all headers except referer
            - referer

        # Send variable values in Apollo in traces sent to Apollo Studio
        send_variable_values: # other possible values are all, only (with an array), except (with an array), none (by default)
            except: # Send all variable values except for variable named first
            - first
    tracing:
        trace_config:
            sampler: 0.5 # The percentage of requests that will generate traces (a rate or `always_on` or `always_off`)
```

By [@BrynCooke](https://github.com/BrynCooke) & [@bnjjj](https://github.com/bnjjj) & [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1514

### Provide access to the supergraph SDL from rhai scripts ([Issue #1735](https://github.com/apollographql/router/issues/1735))

There is a new global constant `apollo_sdl` which can be use to read the
supergraph SDL as a string.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1737

### Add support for `tokio-console` ([PR #1632](https://github.com/apollographql/router/issues/1632))

To aid in debugging the router, this adds support for [tokio-console](https://github.com/tokio-rs/console), enabled by a Cargo feature.

To run the router with tokio-console, build it with `RUSTFLAGS="--cfg tokio_unstable" cargo run --features console`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1632

### Restore the ability to specify custom schema and configuration sources ([#1733](https://github.com/apollographql/router/issues/1733))

You may now, once again, specify custom schema and config sources when constructing an executable.  We had previously omitted this behavior in our API pruning with the expectation that it was still possible to specify via command line arguments and we almost immediately regretted it.  We're happy to have it back!

```rust
Executable::builder()
  .shutdown(ShutdownSource::None)
  .schema(SchemaSource::Stream(schemas))
  .config(ConfigurationSource::Stream(configs))
  .start()
  .await
```
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1734

### Environment variable expansion prefixing ([#1759](https://github.com/apollographql/router/issues/1759))

The environment variable `APOLLO_ROUTER_CONFIG_ENV_PREFIX` can be used to prefix environment variable lookups during configuration expansion. This feature is undocumented and unsupported and may change at any time.  **We do not recommend using this.**

For example:

`APOLLO_ROUTER_CONFIG_ENV_PREFIX=MY_PREFIX`

Would cause:
`${env.FOO}` to be mapped to `${env.MY_PREFIX_FOO}` when expansion is performed.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1763

### Environment variable expansion mode configuration ([#1772](https://github.com/apollographql/router/issues/1772))

The environment variable `APOLLO_ROUTER_CONFIG_SUPPORTED_MODES` can be used to restrict which modes can be used for environment expansion. This feature is undocumented and unsupported and may change at any time.  **We do not recommend using this.**

For example:

`APOLLO_ROUTER_CONFIG_SUPPORTED_MODES=env,file` env and file expansion
`APOLLO_ROUTER_CONFIG_SUPPORTED_MODES=env` - only env variable expansion allowed

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1774


## 🐛 Fixes

### Support execution of the bare `__typename` field ([Issue #1761](https://github.com/apollographql/router/issues/1761))

For queries like `query { __typename }`, we now perform the expected behavior and return a GraphQL response even if the introspection has been disabled.  (`introspection: false` should only apply to _schema introspeciton_ **not** _type-name introspection_.)

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1762

### Set `hasNext` for the last chunk of a deferred response ([#1687](https://github.com/apollographql/router/issues/1687) [#1745](https://github.com/apollographql/router/issues/1745))

There will no longer be an empty last response `{"hasNext": false}` and the `hasNext` field will be set on the last deferred response. There can still be one edge case where that empty message can occur, if some deferred queries were cancelled too quickly.  Generally speaking, clients should expect this to happen to allow future behaviors and this is specified in the `@defer` draft specification.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1687
By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1745

## 🛠 Maintenance

### Add errors vec in `QueryPlannerResponse` to handle errors in `query_planning_service` ([PR #1504](https://github.com/apollographql/router/pull/1504))

We changed `QueryPlannerResponse` to:

- Add a `Vec<apollo_router::graphql::Error>`
- Make the query plan optional, so that it is not present when the query planner encountered a fatal error. Such an error would be in the `Vec`

This should improve the messages returned during query planning.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1504

### Store the Apollo usage reporting Protobuf interface file in the repository

Previously this file was downloaded when compiling the Router, but we had no good way to automatically check when to re-download it without causing the Router to be compiled all the time.

Instead a copy now resides in the repository, with a test checking that it is up to date.  This file can be updated by running this command then sending a PR:

```
curl -f https://usage-reporting.api.apollographql.com/proto/reports.proto \
    > apollo-router/src/spaceport/proto/reports.proto
```

By [@SimonSapin](https://github.com/SimonSapin)

### Disable compression on `multipart/mixed` HTTP responses ([Issue #1572](https://github.com/apollographql/router/issues/1572))

The Router now reverts to using unpatched `async-compression`, and instead disables compression of multipart responses.  We aim to re-enable compression soon, with a proper solution that is being designed in <https://github.com/Nemo157/async-compression/issues/154>.

As context to why we've made this change: features such as `@defer` require the Apollo Router to send a stream of multiple GraphQL responses in a single HTTP response with the body being a single byte stream.  Due to current limitations with our upstream compression library, that entire byte stream is compressed as a whole, which causes the entire deferred response to be held back before being returned.  This obviously isn't ideal for the `@defer` feature which tries to get reponses to client soon possible.

This change replaces our previous work-around which involved a patched `async-compression`, which was not trivial to apply when using the Router as a dependency since [Cargo patching](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html) is done in a project’s root `Cargo.toml`.

Again, we aim to re-visit this as soon as possible but found this to be the more approachable work-around.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1749

## 📚 Documentation
