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

### Respect supergraph path for kubernetes deployment probes (#1787)

For cases where you configured the `supergraph.path` for the router when using the helm chart, the liveness 
and readiness probes continued to use the default path of `/` and so the start failed.

By @damienpontifex in #1788


## 🛠 Maintenance

### Update apollo-router-scaffold to use the published router crate [PR #1782](https://github.com/apollographql/router/pull/1782)

Now that apollo-router version "1.0.0-rc.0" is released on [crates.io](https://crates.io/crates/apollo-router), we can update scaffold to it relies on the published crate instead of the git tag.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1782

### Refactor Configuration validation [#1791](https://github.com/apollographql/router/issues/1791)

Instantiating `Configuration`s is now fallible, because it will run consistency checks on top of the already run structure checks.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1794

## 📚 Documentation
