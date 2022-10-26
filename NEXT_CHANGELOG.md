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

### Follow directives from Uplink ([Issue #1494](https://github.com/apollographql/router/issues/1494) [Issue #1539](https://github.com/apollographql/router/issues/1539))

The Uplink API returns actionable info in its responses:
- some error codes indicate an unrecoverable issue, for which the router should not retry the query (example: non-existing graph)
- it can tell the router when it should retry the query

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2001

## 🛠 Maintenance

### Split the configuration file management in multiple modules [Issue #1790](https://github.com/apollographql/router/issues/1790))

The file is becoming large and hard to modify.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1996

## 📚 Documentation
