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
## 🚀 Features
## 🐛 Fixes

### Prefix the prometheus metrics with `apollo_router_` ([Issue #1915](https://github.com/apollographql/router/issues/1915))

Adopt the prefix naming convention for prometheus metrics.

```diff
- http_requests_error_total{message="cannot contact the subgraph",service_name="apollo-router",subgraph="my_subgraph_name_error",subgraph_error_extended_type="SubrequestHttpError"} 1
+ apollo_router_http_requests_error_total{message="cannot contact the subgraph",service_name="apollo-router",subgraph="my_subgraph_name_error",subgraph_error_extended_type="SubrequestHttpError"} 1
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1971

### Fix a coercion rule that failed to validate 64 bit integers ([PR #1951](https://github.com/apollographql/router/pull/1951))

Queries that passed 64 bit integers for Float values would (incorrectly) fail to validate.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1951

### Set no_delay and keepalive on subgraph requests [Issue #1905](https://github.com/apollographql/router/issues/1905))

It was incorrectly removed in a previous pull request.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1910
