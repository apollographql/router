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

### Fix --hot-reload in kubernetes and docker ([Issue #1476](https://github.com/apollographql/router/issues/1476))

--hot-reload now chooses a file event notification mechanism at runtime. The exact mechanism is determined by the `notify` crate.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1964

### Fix a coercion rule that failed to validate 64 bit integers ([PR #1951](https://github.com/apollographql/router/pull/1951))

Queries that passed 64 bit integers for Float values would (incorrectly) fail to validate.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1951

### Set no_delay and keepalive on subgraph requests [Issue #1905](https://github.com/apollographql/router/issues/1905))

It was incorrectly removed in a previous pull request.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1910

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

## 🛠 Maintenance
## 📚 Documentation
