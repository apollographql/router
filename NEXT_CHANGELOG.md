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

### rhai Context::upsert() supported with example [PR #1136](https://github.com/apollographql/router/pull/1136)

  Rhai plugins can now interact with Context::upsert(). We provide an example (rhai-surrogate-cache-key) to illustrate its use.

### Measure APQ cache hits and registers ([PR #1117](https://github.com/apollographql/router/pull/1117))

  The APQ layer will now report cache hits and misses to Apollo Studio if telemetry is configured

- **Add more information for the subgraph_request span** ([PR #1119](https://github.com/apollographql/router/pull/1119))

  Add a new span only for the subgraph request, with all HTTP and net information needed for the opentelemetry specs

## 🐛 Fixes

### Content-Type is application/json ([1154](https://github.com/apollographql/router/issues/1154)) 
  The router was not setting a content-type on results. This fix ensures that a content-type of application/json is added when returning a graphql response.

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
