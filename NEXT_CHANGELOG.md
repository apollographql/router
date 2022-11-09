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

### **Experimental** 🥼 External cache storage in Redis ([PR #2024](https://github.com/apollographql/router/pull/2024))

implement caching in external storage for query plans, introspection and APQ. This is done as a multi level cache, first in
memory with LRU then with a redis cluster backend. Since it is still experimental, it is opt-in through a Cargo feature.

By [@garypen](https://github.com/garypen) and [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2024

## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
