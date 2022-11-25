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
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [1.8.0] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Remove timeout from otlp exporter ([Issue #2337](https://github.com/apollographql/router/issues/2337))

`batch_processor` configuration contains timeout, so the existing timeout property has been removed from the parent configuration element.

Before:
```yaml
telemetry:
  tracing:
    otlp:
      timeout: 5s
```
After:
```yaml
telemetry:
  tracing:
    otlp:
      batch_processor:
        timeout: 5s
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2338

## 🚀 Features
### Configuration upgrades ([Issue #2124](https://github.com/apollographql/router/issues/2124))

The Router will now send anonymous usage telemetry to Apollo that includes a subset of the information listed in the [privacy policy](https://www.apollographql.com/docs/router/privacy/)

It includes information about the environment that the Router is running in, version of the router, the command line args, and configuration shape.
Note that strings are output as `set` so that we do not leak confidential or sensitive information. 
Boolean and numerics are output.

For example:
```json
{
   "session_id": "fbe09da3-ebdb-4863-8086-feb97464b8d7",
   "version": "1.4.0", // The version of the router
   "os": "linux",
   "ci": null,
   "supergraph_hash": "anebfoiwhefowiefj",
   "apollo-key": "<the actualy key>|anonymous",
   "apollo-graph-ref": "<the actual graph ref>|unmanaged"
   "usage": {
     "configuration.headers.all.request.propagate.named.redacted": 3
     "configuration.headers.all.request.propagate.default.redacted": 1
     "configuration.headers.all.request.len": 3
     "configuration.headers.subgraphs.redacted.request.propagate.named.redacted": 2
     "configuration.headers.subgraphs.redacted.request.len": 2
     "configuration.headers.subgraphs.len": 1
     "configuration.homepage.enabled.true": 1
     "args.config-path.redacted": 1,
     "args.hot-reload.true": 1,
     //Many more keys. This is dynamic and will change over time.
     //More...
     //More...
     //More...
   }
 }
```

Users can disable the sending this data by using the command line flag `--anonymous-telemetry-disabled` or setting the environment variable `APOLLO_TELEMETRY_DISABLED=true`

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2173

### Add support for single instance Redis ([Issue #2300](https://github.com/apollographql/router/issues/2300))

For `experimental_cache` with redis caching it now works with only a single Redis instance if you provide only one URL.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2310

## 🐛 Fixes

### Change the default value of `apollo.field_level_instrumentation_sampler` ([Issue #2339](https://github.com/apollographql/router/issues/2339))

Change the default value of `apollo.field_level_instrumentation_sampler` to `always_off` instead of `0.01`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2356

### `subgraph_request` span is set as the parent of traces coming from subgraphs ([Issue #2344](https://github.com/apollographql/router/issues/2344))

Before this fix, the context injected in headers to subgraphs was wrong, it was not the right parent span id.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2345


## 🛠 Maintenance

### Simplify telemetry config code ([Issue #2337](https://github.com/apollographql/router/issues/2337))

This brings the telemetry plugin configuration closer to standards recommended in the [yaml design guidance](dev-docs/yaml-design-guidance.md).

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2338

### Upgrade the clap version in scaffold template ([Issue #2165](https://github.com/apollographql/router/issues/2165))

Upgrade clap deps version to the right one to be able to create new scaffolded plugins thanks to xtask.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2343

### Upgrade axum to `0.6.1` ([PR #2303](https://github.com/apollographql/router/pull/2303))

For more details about the new axum release, please read the [changelog](https://github.com/tokio-rs/axum/releases/tag/axum-v0.6.0)

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2303

### Specify content type to `application/json` when it throws an invalid GraphQL request error ([Issue #2320](https://github.com/apollographql/router/issues/2320))

When throwing a `INVALID_GRAPHQL_REQUEST` error, it now specifies the right `content-type` header.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2321

### Move APQ and EnsureQueryPresence in the router service ([PR #2296](https://github.com/apollographql/router/pull/2296))

Moving APQ from the axum level to the supergraph service reintroduced a `Buffer` in the service pipeline.
Now the APQ and`EnsureQueryPresence ` layers are part of the router service, to remove that `Buffer`.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2296
