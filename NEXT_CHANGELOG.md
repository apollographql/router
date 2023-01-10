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

### JWT authentication for the router ([Issue #912](https://github.com/apollographql/router/issues/912))

JWT authentication is now configurable for the router.

Here's a typical sample configuration fragment:

```yaml
authentication:
  jwt:
    jwks_url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
```

Until the documentation is published to the website, you can read [it](https://github.com/apollographql/router/blob/53d7b710a6bdc0fbef4d7fd0d13f49002ee70e84/docs/source/configuration/authn-jwt.mdx) from the pull request.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2348

### Add cache hit/miss metrics ([Issue #1985](https://github.com/apollographql/router/issues/1985))

Add several metrics around the cache.
Each cache metrics it contains `kind` attribute to know what kind of cache it was (`query planner`, `apq`, `introspection`)
and the `storage` attribute to know where the cache is coming from.

`apollo_router_cache_hit_count` to know when it hits the cache.

`apollo_router_cache_miss_count` to know when it misses the cache.

`apollo_router_cache_hit_time` to know how much time it takes when it hits the cache.

`apollo_router_cache_miss_time` to know how much time it takes when it misses the cache.

Example
```
# TYPE apollo_router_cache_hit_count counter
apollo_router_cache_hit_count{kind="query planner",new_test="my_version",service_name="apollo-router",storage="memory"} 2
# TYPE apollo_router_cache_hit_time histogram
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.001"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.005"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.015"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.05"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.1"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.2"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.3"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.4"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.5"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="1"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="5"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="10"} 2
apollo_router_cache_hit_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="+Inf"} 2
apollo_router_cache_hit_time_sum{kind="query planner",service_name="apollo-router",storage="memory"} 0.000236782
apollo_router_cache_hit_time_count{kind="query planner",service_name="apollo-router",storage="memory"} 2
# HELP apollo_router_cache_miss_count apollo_router_cache_miss_count
# TYPE apollo_router_cache_miss_count counter
apollo_router_cache_miss_count{kind="query planner",service_name="apollo-router",storage="memory"} 1
# HELP apollo_router_cache_miss_time apollo_router_cache_miss_time
# TYPE apollo_router_cache_miss_time histogram
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.001"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.005"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.015"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.05"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.1"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.2"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.3"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.4"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="0.5"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="1"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="5"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="10"} 1
apollo_router_cache_miss_time_bucket{kind="query planner",service_name="apollo-router",storage="memory",le="+Inf"} 1
apollo_router_cache_miss_time_sum{kind="query planner",service_name="apollo-router",storage="memory"} 0.000186783
apollo_router_cache_miss_time_count{kind="query planner",service_name="apollo-router",storage="memory"} 1
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2327

### Add support for single instance Redis ([Issue #2300](https://github.com/apollographql/router/issues/2300))

For `experimental_cache` with redis caching it now works with only a single Redis instance if you provide only one URL.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2310

## 🐛 Fixes

### Correctly handle aliased __typename ([Issue #2330](https://github.com/apollographql/router/issues/2330))

If you aliased a `__typename` like in this example query:

```graphql
{
  myproducts: products {
       total
       __typename
  }
  _0___typename: __typename
}
```

Before this fix, `_0___typename` was set to `null`. Thanks to this fix it returns `"Query"`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2357

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

### Refactor yaml validation error reports ([Issue #2180](https://github.com/apollographql/router/issues/2180))

YAML configuration file validation prints a report of the errors it encountered, but that report was missing some
information, and had small mistakes around alignment and pointing out the right line.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2347
