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

## 🚀 Features

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

## 🛠 Maintenance

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
