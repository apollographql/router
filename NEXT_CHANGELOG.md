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

### Add support for `tokio-console` ([PR #1632](https://github.com/apollographql/router/issues/1632))

to aid in debugging the router, this adds support for [tokio-console](https://github.com/tokio-rs/console), enabled by a Cargo feature.

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
