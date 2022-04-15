# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

- **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

  Description! And a link to a [reference](http://url)
-->

# [v0.1.0-preview.5] - (unreleased)
## ❗ BREAKING ❗
## 🚀 Features
### Line precise error reporting [PR #830](https://github.com/apollographql/router/pull/782)
The router will make a best effort to give line precise error reporting if the configuration was invalid.
```yaml
1. /telemetry/tracing/trace_config/sampler

telemetry:
  tracing:
    trace_config:
      service_name: router3
      sampler: "0.3"
               ^----- "0.3" is not valid under any of the given schemas
```
### Install experience [PR #820](https://github.com/apollographql/router/pull/820)

  Added an install script that will automatically download and unzip the router into the local directory.
  For more info see the quickstart documentation.

## 🐛 Fixes

### Make sure concurrent query plan are not mixed up [#846](https://github.com/apollographql/router/issues/846)
The query planner has been reworked to make sure concurrent plan requests will be dispatched to the relevant requester.

### Do not hang when tracing provider was not set as global [#849](https://github.com/apollographql/router/issues/847)
The telemetry plugin will now Drop cleanly when the Router service stack fails to build.

### Propagate error extensions originating from subgraphs [PR #839](https://github.com/apollographql/router/pull/839)
Extensions are now propagated following the configuration of the `include_subgraph_error` plugin.

### Telemetry configuration [PR #830](https://github.com/apollographql/router/pull/830)
Jaeger and Zipkin telemetry config produced JSON schema that was invalid.

### Early return a better error when introspection is disabled [PR #751](https://github.com/apollographql/router/pull/751)
Instead of returning an error coming from the query planner, we are now returning a proper error explaining that the introspection has been disabled.

### Add operation name to subquery fetches [PR #840](https://github.com/apollographql/router/pull/840)
If present in the query plan fetch noede, the operation name will be added to sub-fetches.

## 🛠 Maintenance
### Configuration files validated [PR #830](https://github.com/apollographql/router/pull/830)
Router configuration files within the project are now largely validated via unit test.

### Switch web server framework from `warp` to `axum` [PR #751](https://github.com/apollographql/router/pull/751)
The router is now running by default with an [axum](https://github.com/tokio-rs/axum/) web server instead of `warp`.

### Improve the way we handle Request with axum [PR #845](https://github.com/apollographql/router/pull/845)
Take advantages of new extractors given by `axum`
  
## 📚 Documentation
