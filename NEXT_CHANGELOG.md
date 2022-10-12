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

### Fix logic around Accept headers and multipart ([PR #1923](https://github.com/apollographql/router/pull/1923))

If the Accept header contained `multipart/mixed`, even with other alternatives like `application/json`,
a query with a single response was still sent as multipart, which made Explorer fail on the initial
introspection query.

This changes the logic so that:

* if we accept application/json or wildcard and there's a single response, it comes as json
* if there are multiple responses or we only accept multipart, send a multipart responses
* otherwise return a HTTP 406 Not Acceptable

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1923

## 🛠 Maintenance
## 📚 Documentation
