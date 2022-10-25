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

### Fix OTLP GRPC ([Issue #1976](https://github.com/apollographql/router/issues/1976))

OTLP GRPC has been fixed and confirmed to work against external APMs.

* TLS root certificates needed to be enabled in tonic.
* TLS domain needs to be set for GRPC over HTTP(S). THis is now defaulted.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/#1977

### Prefix the prometheus metrics with `apollo_router_` ([Issue #1915](https://github.com/apollographql/router/issues/1915))

Adopt the prefix naming convention for prometheus metrics.

```diff
- http_requests_error_total{message="cannot contact the subgraph",service_name="apollo-router",subgraph="my_subgraph_name_error",subgraph_error_extended_type="SubrequestHttpError"} 1
+ apollo_router_http_requests_error_total{message="cannot contact the subgraph",service_name="apollo-router",subgraph="my_subgraph_name_error",subgraph_error_extended_type="SubrequestHttpError"} 1
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1971 & https://github.com/apollographql/router/pull/1987

### Fix --hot-reload in kubernetes and docker ([Issue #1476](https://github.com/apollographql/router/issues/1476))

--hot-reload now chooses a file event notification mechanism at runtime. The exact mechanism is determined by the `notify` crate.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1964

### Fix a coercion rule that failed to validate 64 bit integers ([PR #1951](https://github.com/apollographql/router/pull/1951))

Queries that passed 64 bit integers for Float values would (incorrectly) fail to validate.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1951

### prometheus: make sure http_requests_error_total and http_requests_total are incremented. ([PR #1953](https://github.com/apollographql/router/pull/1953))

`http_requests_error_total` did only increment for requests that would be an `INTERNAL_SERVER_ERROR` in the router (the service stack returning a BoxError).
What this means is that validation errors would not increment this counter.

`http_requests_total` would only increment for successful requests, while the prometheus documentation mentions this key should be incremented regardless of whether the request succeeded or not.

This PR makes sure we always increment `http_requests_total`, and we increment `http_requests_error_total` when the StatusCode is not 2XX.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1953

### Set no_delay and keepalive on subgraph requests [Issue #1905](https://github.com/apollographql/router/issues/1905))

It was incorrectly removed in a previous pull request.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1910

### Assume `Accept: application/json` when no `Accept` header is present [Issue #1995](https://github.com/apollographql/router/pull/1995))

the `Accept` header means `*/*` when it is absent.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1995

## 🛠 Maintenance

### Update to Federation v2.1.4 ([PR #1994](https://github.com/apollographql/router/pull/1994))

In addition to general Federation bug-fixes, this update should resolve a case ([seen in Issue #1962](https://github.com/apollographql/router/issues/1962)) where a `@defer` directives which had been previously present in a Supergraph were causing a startup failure in the Router when we were trying to generate an API schema in the Router with `@defer`.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1994

### Improve the stability of some flaky tests ([PR #1972](https://github.com/apollographql/router/pull/1972))

The trace and rate limiting tests have been failing in our ci environment. The root cause is racyness in the tests, so the tests have been made more resilient to reduce the number of failures.

Two PRs are represented by this single changelog.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1972
By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1974

### Update docker-compose and Dockerfiles now that the submodules have been removed ([PR #1950](https://github.com/apollographql/router/pull/1950))

We recently removed git submodules dependency, but we didn't update the global and the fuzzer `docker-compose.yml`.

This PR adds new Dockerfiles and updates `docker-compose.yml` so we can run integration tests and the fuzzer without needing to clone and set up the federation and fed2-demo repositories.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1950

### Fix logic around Accept headers and multipart ([PR #1923](https://github.com/apollographql/router/pull/1923))

If the Accept header contained `multipart/mixed`, even with other alternatives like `application/json`,
a query with a single response was still sent as multipart, which made Explorer fail on the initial
introspection query.

This changes the logic so that:

* if we accept application/json or wildcard and there's a single response, it comes as json
* if there are multiple responses or we only accept multipart, send a multipart responses
* otherwise return a HTTP 406 Not Acceptable

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1923

### `@defer`: duplicated errors across incremental items ([Issue #1834](https://github.com/apollographql/router/issues/1834), [Issue #1818](https://github.com/apollographql/router/issues/1818))

If a deferred response contains incremental responses, the errors should be dispatched in each increment according to the
error's path.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1892

### Adding `image.source` label to docker image

Adding docker source label, published images will be linked to github repo's packages section

By [@ndthanhdev](https://github.com/ndthanhdev) in https://github.com/apollographql/router/pull/1958

### Split the configuration file management in multiple modules [Issue #1790](https://github.com/apollographql/router/issues/1790))

The file is becoming large and hard to modify.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1996
## 📚 Documentation
