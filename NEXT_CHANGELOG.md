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

### **Headline** ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [0.12.1] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Remove the generic stream type from RouterResponse and ExecutionResponse ([PR #1420](https://github.com/apollographql/router/pull/1420)

This generic type complicates the API with limited benefit because we use BoxStream everywhere in plugins:
* `RouterResponse<BoxStream<'static, Response>>` -> `RouterResponse`
* `ExecutionResponse<BoxStream<'static, Response>>` -> `ExecutionResponse`

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1420

## 🚀 Features

## 🐛 Fixes

### Update the scaffold template so it targets router v0.12.0 ([#PR1431](https://github.com/apollographql/router/pull/1431))

The cargo scaffold template will target the latest version of the router.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1248

### **A Rhai error instead of a Rust panic** ([PR #1414 https://github.com/apollographql/router/pull/1414)

In Rhai plugins, accessors that mutate the originating request are not available when in the subgraph phase. Previously trying to mutate anyway would cause a Rust panic. This has been changed to a Rhai error instead.

By @SimonSapin

## 🛠 Maintenance

## 📚 Documentation
