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

### Add support for urlencode/decode to rhai engine ([Issue #2052](https://github.com/apollographql/router/issues/2052))

Two new functions, `urlencode()` and `urldecode()` may now be used to urlencode/decode strings.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2053

### **Experimental** 🥼 External cache storage in Redis ([PR #2024](https://github.com/apollographql/router/pull/2024))

implement caching in external storage for query plans, introspection and APQ. This is done as a multi level cache, first in
memory with LRU then with a redis cluster backend. Since it is still experimental, it is opt-in through a Cargo feature.

By [@garypen](https://github.com/garypen) and [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2024

## 🐛 Fixes

### Fix `Float` input-type coercion for default values with values larger than 32-bits ([Issue #2087](https://github.com/apollographql/router/issues/2087))

A regression has been fixed which caused the Router to reject integers larger than 32-bits used as the default values on `Float` fields in input types.

In other words, the following will once again work as expected:

```graphql
input MyInputType {
    a_float_input: Float = 9876543210
}
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2090

### Assume `Accept: application/json` when no `Accept` header is present [Issue #1990](https://github.com/apollographql/router/issues/1990))

the `Accept` header means `*/*` when it is absent.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2078

### Missing `@skip` and `@include` implementation for root operations ([Issue #2072](https://github.com/apollographql/router/issues/2072))

`@skip` and `@include` were not implemented for inline fragments and fragment spreads on top level operations.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2096

## 🛠 Maintenance
## 📚 Documentation

### Fix example `helm show values` command ([PR #2088](https://github.com/apollographql/router/pull/2088))

The `helm show vaues` command needs to use the correct Helm chart reference `oci://ghcr.io/apollographql/helm-charts/router`.

By [@col](https://github.com/col) in https://github.com/apollographql/router/pull/2088
