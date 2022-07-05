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

# [0.10.1] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗

### Relax plugin api mutability ([PR #1340](https://github.com/apollographql/router/pull/1340) ([PR #1289](https://github.com/apollographql/router/pull/1289)

the `Plugin::*_service()` methods were taking a `&mut self` as argument, but since
they work like a tower Layer, they can use `&self` instead. This change
then allows us to move from Buffer to service factories for the query
planner, execution and subgraph services

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1340 https://github.com/apollographql/router/pull/1289

## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance

### Replace Buffers of tower services with service factories([PR #1289](https://github.com/apollographql/router/pull/1289)

tower services should be used by creating a new service instance for each new session
instead of going through a Buffer.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1289

## 📚 Documentation
