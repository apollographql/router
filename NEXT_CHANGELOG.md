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

# [0.12.1] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Modify the plugin `new` method to pass an initialisation structure ([PR #1446](https://github.com/apollographql/router/pull/1446))

This change alters the `new` method for plugins to pass a `PluginInit` struct.

We are making this change so that we can pass more information during plugin startup. The first change is that in addition to passing
the plugin configuration, we are now also passing the router supergraph sdl (Schema Definition Language) as a string.

There is a new example (`supergraph_sdl`) which illustrates how to use this new capability.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1446

### Remove the generic stream type from `RouterResponse` and `ExecutionResponse` ([PR #1420](https://github.com/apollographql/router/pull/1420))

This generic type complicates the API with limited benefit because we use `BoxStream` everywhere in plugins:

* `RouterResponse<BoxStream<'static, Response>>` -> `RouterResponse`
* `ExecutionResponse<BoxStream<'static, Response>>` -> `ExecutionResponse`

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1420

### Remove the HTTP request from `QueryPlannerRequest` ([PR #1439](https://github.com/apollographql/router/pull/1439))

The content of `QueryPlannerRequest` is used as argument to the query planner and as a cache key,
so it should not change depending on the variables or HTTP headers.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1439

## 🚀 Features

### Publish helm chart to OCI registry ([PR #1447](https://github.com/apollographql/router/pull/1447))

When we make a release, publish our helm chart to the same OCI registry that we use for our docker images.

For more information about using OCI registries with helm, see [the helm documentation](https://helm.sh/blog/storing-charts-in-oci/).

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1447

### Configure Regex based CORS rules ([PR #1444](https://github.com/apollographql/router/pull/1444))

The router now supports regex based CORS rules, as explained in the [docs](https://www.apollographql.com/docs/router/configuration/cors)
It also supports the `allow_any_header` setting that will mirror client's requested headers.

```yaml title="router.yaml"
server:
  cors:
    match_origins:
      - "https://([a-z0-9]+[.])*api[.]example[.]com" # any host that uses https and ends with .api.example.com
    allow_any_header: true # mirror client's headers
```

The default CORS headers configuration of the router allows `content-type`, `apollographql-client-version` and `apollographql-client-name`.

By [@o0Ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/1444


### Add support of error section in telemetry to add custom attributes ([PR #1443](https://github.com/apollographql/router/pull/1443))

The telemetry is now able to hook at the error stage if router or a subgraph is returning an error. Here is an example of configuration:

```yaml
telemetry:
  metrics:
    prometheus:
      enabled: true
    common:
      attributes:
        subgraph:
          all:
            errors: # Only works if it's a valid GraphQL error
              include_messages: true # Will include the error message in a message attribute
              extensions: # Include extension data
                - name: subgraph_error_extended_type # Name of the attribute
                  path: .type # JSON query path to fetch data from extensions
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1443

### Experimental support for the `@defer` directive ([PR #1182](https://github.com/apollographql/router/pull/1182))

The router can now understand the `@defer` directive, used to tag parts of a query so the response is split into
multiple parts that are sent one by one.

:warning: *this is still experimental and not fit for production use yet*

To activate it, add this option to the configuration file:

```yaml
server:
  experimental_defer_support: true
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1182

### Rewrite the caching API ([PR #1281](https://github.com/apollographql/router/pull/1281))

This introduces a new asynchronous caching API that opens the way to multi level caching (in memory and
database). The API revolves around an `Entry` structure that allows query deduplication and lets the
client decide how to generate the value to cache, instead of a complicated delegate system inside the
cache.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1281

## 🐛 Fixes

### Update serialization format for telemetry.tracing.otlp.grpc.metadata ([ PR #1391](https://github.com/apollographql/router/pull/1391))

The metadata format now uses `IndexMap<String, Vec<String>>`.

By [@me-diru](https://github.com/me-diru) in https://github.com/apollographql/router/pull/1391 

### Update the scaffold template so it targets router v0.12.0 ([PR #1431](https://github.com/apollographql/router/pull/1431))

The cargo scaffold template will target the latest version of the router.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1248

### Selection merging on non-object field aliases ([PR #1406](https://github.com/apollographql/router/issues/1406))

Fixed a bug where merging aliased fields would sometimes put `null`s instead of expected values. 

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1432

### A Rhai error instead of a Rust panic ([PR #1414 https://github.com/apollographql/router/pull/1414))

In Rhai plugins, accessors that mutate the originating request are not available when in the subgraph phase. Previously, trying to mutate anyway would cause a Rust panic. This has been changed to a Rhai error instead.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1414

### Optimizations ([PR #1423](https://github.com/apollographql/router/pull/1423))

* Do not clone the client request during query plan execution
* Do not clone the usage reporting
* Avoid path allocations when iterating over JSON values

The benchmarks show that this change brings a 23% gain in requests per second compared to the main branch.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1423

### do not perform nested fetches if the parent one returned null ([PR #1332](https://github.com/apollographql/router/pull/1332)

In a query of the form:
```graphql
mutation {
	mutationA {
		mutationB
	}
}
```

If `mutationA` returned null, we should not execute `mutationB`.

By [@Ty3uK](https://github.com/Ty3uK) in https://github.com/apollographql/router/pull/1332

## 🛠 Maintenance

## 📚 Documentation

### Updates wording and formatting of README.md

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/1445
