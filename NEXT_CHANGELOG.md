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

### Fix float input default value coercion on big integers ([Issue #2087](https://github.com/apollographql/router/issues/2087))

The router will now correctly accept integers that dont fit in 32 bits as Float default values:

A supergraph schema that contains:
```graphql
    input MyInputType {
        a_float_input: Float = 9876543210
    }
```

is not correctly accepted by the router.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2090

## 🛠 Maintenance
## 📚 Documentation
