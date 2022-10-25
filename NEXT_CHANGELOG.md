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

### Assume `Accept: application/json` when no `Accept` header is present [Issue #1995](https://github.com/apollographql/router/pull/1995))

the `Accept` header means `*/*` when it is absent.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1995

## 🛠 Maintenance

### Update to Federation v2.1.4 ([PR #1994](https://github.com/apollographql/router/pull/1994))

In addition to general Federation bug-fixes, this update should resolve a case ([seen in Issue #1962](https://github.com/apollographql/router/issues/1962)) where a `@defer` directives which had been previously present in a Supergraph were causing a startup failure in the Router when we were trying to generate an API schema in the Router with `@defer`.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1994

## 📚 Documentation
