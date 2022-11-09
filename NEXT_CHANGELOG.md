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

### Move the nullifying error messages to extension ([Issue #2071](https://github.com/apollographql/router/issues/2071))

The Router was generating error messages when triggering nullability rules (when a non nullable field is null,
it will nullify the parent object). Adding those messages in the list of errors was potentially redundant
(subgraph can already add an error message indicating why a field is null) and could be treated as a failure by
clients, while nullifying fields is a part of normal operation. We still add the messages in extensions so
clients can easily debug why parts of the response were removed

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2077

## 🛠 Maintenance
## 📚 Documentation
