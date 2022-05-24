# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

### **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

Description! And a link to a [reference](http://url)
-->

# [0.9.3] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

## 🚀 Features
### Scaffold custom binary support ([PR #1104](https://github.com/apollographql/router/pull/1104))
Added CLI support for scaffolding a new Router binary project. This provides a starting point for people who want to use the Router as a library and create their own plugins

- **Measure APQ cache hits and registers** ([PR #1117](https://github.com/apollographql/router/pull/1117))

  The APQ layer will now report cache hits and misses to Apollo Studio if telemetry is configured

## 🐛 Fixes

- **Prevent memory leaks when tasks are cancelled** [PR #767](https://github.com/apollographql/router/pull/767)

  Cancelling a request could put the router in an unresponsive state where the deduplication layer or cache would make subgraph requests hang.

## 🛠 Maintenance
### Unpin schemars version [#1074](https://github.com/apollographql/router/issues/1074)
The Schemars 0.8.9 caused compile errors due to it validating default types.
This change has however been rolled back upstream.
We can now safely depend on schemars 0.8.10.

### Update Moka to fix occasional panics on AMD hardware [#1137](https://github.com/apollographql/router/issues/1137)
Moka has a dependency on Quanta which had an issue with AMD hardware. This is now fixed via [Moka-#119](https://github.com/moka-rs/moka/issues/119).

## 📚 Documentation

## 🐛 Fixes
