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

## 🐛 Fixes

### Fix a coercion rule that failed to validate 64 bit integers ([PR #1951](https://github.com/apollographql/router/pull/1951))

Queries that passed 64 bit integers for Float values would (incorrectly) fail to validate.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1951

### prometheus: make sure http_requests_error_total and http_requests_total are incremented. ([PR #1953](https://github.com/apollographql/router/pull/1953))

`http_requests_error_total` did only increment for requests that would be an `INTERNAL_SERVER_ERROR` in the router (the service stack returning a BoxError).
What this means is that validation errors would not increment this counter.

`http_requests_total` would only increment for successful requests, while the prometheus documentation mentions this key should be incremented regardless of whether the request succeeded or not.

This PR makes sure we always increment `http_requests_total`, and we increment `http_requests_error_total` when the StatusCode is not 2XX.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1953