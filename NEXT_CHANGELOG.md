# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 📃 Configuration
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [1.8.1] (unreleased) - 2022-mm-dd

## 🛠 Maintenance

### Remove unused factory traits ([Issue #2180](https://github.com/apollographql/router/pull/2372))

Building the execution and subgraph services had to go through a factory trait before, which is not
needed anymore since there is only one useful implementation.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2372

### Optimize header propagation plugin's regex matching ([PR #2391](https://github.com/apollographql/router/pull/2389))

We've changed the plugin to reduce the chances of generating memory allocations when applying regex-based header propagation rules.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2389
