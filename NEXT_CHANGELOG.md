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

### Improve errors when subgraph returns non-GraphQL response with a non-2xx status code ([Issue #2117](https://github.com/apollographql/router/issues/2117))

The error response will now contain the status code and status name. Example: `HTTP fetch failed from 'my-service': 401 Unauthorized`

By [@col](https://github.com/col) in https://github.com/apollographql/router/pull/2118

## 🛠 Maintenance
## 📚 Documentation
