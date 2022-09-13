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

### Different default value for `sandbox` and `introspection` configuration ([PR #1748](https://github.com/apollographql/router/pull/1748))

By default, `sandbox` and `introspection` configuration are disabled. You have to force it in your configuration file with:

```yaml
sandbox:
  # ...
  enabled: true
supergraph:
    # ...
  introspection: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1748

### Configuration: Update metrics and healthcheck web endpoints, and make them configurable ([#1500](https://github.com/apollographql/router/issues/1500))

The web endpoints exposed by the router listen to 127.0.0.1 by default, and the ports and paths for health check and prometheus have changed.

Here's the list of the endpoints exposed by the router:

- GraphQL: http://127.0.0.1:4000/ (unchanged)
- The GraphQL sandbox: http://127.0.0.1:4000/ (unchanged)
- Prometheus metrics: http://127.0.0.1:9090/metrics (used to be http://127.0.0.1:4000/plugins/apollo.telemetry/prometheus)
- Healthcheck: http://127.0.0.1:9494/health (used to be http://127.0.0.1:4000/.well-known/apollo/server-health)

While you could previously only customize the path for these endpoints, you can now customize the full IP address, PORT and PATH.

In order to enable this new feature, various `server` attributes such as `listen`, `graphql_path` and `landing_page` moved to more relevant sections.
Likewise, `introspection` and `preview_defer_support` have moved from the `server` section to the `supergraph` section:

This previous configuration: 
```yaml
server:
  listen: 127.0.0.1:4000
  graphql_path: /graphql
  health_check_path: /health
  introspection: false
  preview_defer_support: true
  landing_page: true
telemetry:
  metrics:
    prometheus:
      enabled: true
```

Now becomes:
```yaml
# landing_page configuration
sandbox: 
  listen: 127.0.0.1:4000
  path: /
  enabled: false # default
# graphql_path configuration
supergraph:
  listen: 127.0.0.1:4000
  path: /
  introspection: false
  preview_defer_support: true
# health_check_path configuration
health-check:
  listen: 127.0.0.1:9494
  path: /health
  enabled: true # default
# prometheus scraper configuration
telemetry:
  metrics:
    prometheus:
      listen: 127.0.0.1:9090
      path: /metrics
      enabled: true
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1718

### `apollo-spaceport` and `uplink` are now part of `apollo-router` ([Issue #491](https://github.com/apollographql/router/issues/491))

Instead of being dependencies, they are now part of the `apollo-router` crate.
Therefore, they can not longer be used separately.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1751

### Remove over-exposed functions from the public API ([PR #1746](https://github.com/apollographql/router/pull/1746))

The following functions are only required for router implementation, so removing from external API.
subgraph::new_from_response
supergraph::new_from_response
supergraph::new_from_graphql_response

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1746

### Span client_name and client_version attributes renamed ([#1514](https://github.com/apollographql/router/issues/1514))
OpenTelemetry attributes should be grouped by `.` rather than `_`, therefore the following attributes have changed:

* `client_name` => `client.name`
* `client_version` => `client.version`

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1514

## 🚀 Features

### Add support of query resolution with single `__typename` field ([Issue #1761](https://github.com/apollographql/router/issues/1761))

For queries like `query { __typename }`, we added support to returns a GraphQL response even if the introspection has been disabled

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1762

### Provide access to the supergraph SDL from rhai scripts ([Issue #1735](https://github.com/apollographql/router/issues/1735))

There is a new global constant `apollo_sdl` which can be use to read the
supergraph SDL as a string.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1737

### Add federated tracing support to Apollo studio usage reporting ([#1514](https://github.com/apollographql/router/issues/1514))

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

### Adds a development mode that can be enabled with the `--dev` flag ([#1474](https://github.com/apollographql/router/issues/1474))

By default, the Apollo Router is configured with production best-practices.  When developing, it is often desired to have some of those features relaxed to make it easier to iterate.  A `--dev` flag has been introduced to make the user experience easier while maintaining a default configuration which targets a productionized environment.

The `--dev` mode will enable a few options _for development_ which are not normally on by default:

- Introspection will be enabled, allowing client tooling to obtain the latest version of the schema.
- The Apollo Sandbox Explorer will be served instead of the Apollo Router landing page, allowing you to run queries against your development Router.
- Hot-reloading of configuration will be enabled.
- It will be possible for Apollo Sandbox Explorer to request a query plan to be returned with any operations it executes. These query plans will allow you to observe how the operation will be executed against the underlying subgraphs.
- Errors received from subgraphs will not have their contents redacted to facilitate debugging.

Additional considerations will be made in the future as we introduce new features that might necessitate a "development" workflow which is different than the default mode of operation.  We will try to minimize these differences to avoid surprises in a production deployment while providing an execellent development experience.  In the future, the (upcoming) `rover dev` experience will become our suggested pattern, but this should serve the purpose in the near term.

By [@bnjjj](https://github.com/bnjjj) and [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) and [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1748

### Add support for `tokio-console` ([PR #1632](https://github.com/apollographql/router/issues/1632))

To aid in debugging the router, this adds support for [tokio-console](https://github.com/tokio-rs/console), enabled by a Cargo feature.

To run the router with tokio-console, build it with `RUSTFLAGS="--cfg tokio_unstable" cargo run --features console`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1632

### Restore the ability to specify custom schema and configuration sources ([#1733](https://github.com/apollographql/router/issues/1733))
You may now specify custom schema and config sources when constructing an executable.
```rust
Executable::builder()
  .shutdown(ShutdownSource::None)
  .schema(SchemaSource::Stream(schemas))
  .config(ConfigurationSource::Stream(configs))
  .start()
  .await
```
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1734

## 🐛 Fixes

### Set correctly hasNext for the last chunk of a deferred response ([#1687](https://github.com/apollographql/router/issues/1687))

You no longer will receive a last chunk `{"hasNext": false}` in a deferred response.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1736

## 🛠 Maintenance

### Add errors vec in `QueryPlannerResponse` to handle errors in `query_planning_service` ([PR #1504](https://github.com/apollographql/router/pull/1504))

We changed `QueryPlannerResponse` to:

+ Add a `Vec<apollo_router::graphql::Error>`
+ Make the query plan optional, so that it is not present when the query planner encountered a fatal error. Such an error would be in the `Vec`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1504

### Disable compression of multipart HTTP responses ([Issue #1572](https://github.com/apollographql/router/issues/1572))

For features such a `@defer`, the Router may send a stream of multiple GraphQL responses
in a single HTTP response.
The body of the HTTP response is a single byte stream.
When HTTP compression is used, that byte stream is compressed as a whole.
Due to limitations in current versions of the `async-compression` crate,
[issue #1572](https://github.com/apollographql/router/issues/1572) was a bug where
some GraphQL responses might not be sent to the client until more of them became available.
This buffering yields better compression, but defeats the point of `@defer`.

Our previous work-around involved a patched `async-compression`,
which was not trivial to apply when using the Router as a dependency
since [Cargo patching](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html)
is done in a project’s root `Cargo.toml`.

The Router now reverts to using unpatched `async-compression`,
and instead disables compression of multipart responses.
We aim to re-enable compression soon, with a proper solution that is being designed in
<https://github.com/Nemo157/async-compression/issues/154>.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1749

## 📚 Documentation
