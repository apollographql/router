# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <KEEP> THIS IS AN SET OF TEMPLATES TO USE WHEN ADDING TO THE CHANGELOG.

## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 📃 Configuration
Configuration changes will be [automatically migrated on load](https://www.apollographql.com/docs/router/configuration/overview#upgrading-your-router-configuration). However, you should update your source configuration files as these will become breaking changes in a future major release.
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
</KEEP> -->

## 🚀 Features

### Subgraph entity caching ([PR #2526](https://github.com/apollographql/router/pull/2526))

First pass implementation of subgraph entity caching. This will cache individual queries returned by
federated queries (not root operations), separated in the cache by type, key, subgraph query,
root operation and variables.
This is only an in memory LRU cache with 1024 entries, and does not support invalidation.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2526
