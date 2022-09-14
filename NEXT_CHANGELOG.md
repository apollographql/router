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


### ### Do not check for listen adddress conflicts with a disabled sandbox. [PR #1781](https://github.com/apollographql/router/pull/1781)

Setting `supergraph.listen` to `0.0.0.0:4000` would conflict with the sandbox's default `127.0.0.1:4000` regardless of whether the sandbox is enabled or not.

A workaround before next release is to change `sandbox.listen` to either match `supergraph.listen` or bind it to an other port:

```yaml
supergraph:
  listen: 0.0.0.0:4000
sandbox:
  listen: 0.0.0.0:4000
  enabled: false
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1781

## 🛠 Maintenance
## 📚 Documentation
