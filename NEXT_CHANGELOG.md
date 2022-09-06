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

### register_plugin now restricts contents of group and name parameters ([#1704](https://github.com/apollographql/router/issues/1703))

At startup, the router registers plugins. The crate we used to use (`startup`) has been yanked from crates.io. We've decided to move to the `ctor` crate. As part of this move, we now generate plugin registration names and this requires that the "group" and "name" parameters only contain characters which would be valid in the [composition of a rust function name](https://doc.rust-lang.org/stable/reference/identifiers.html).

In practice, this is extremely unlikely to break any existing plugins, but it is a breaking change.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1704

## 🚀 Features
## 🐛 Fixes

### Fix telemetry propagation with headers ([#1701](https://github.com/apollographql/router/issues/1701))

Span context is now correctly propagated if you're trying to propagate tracing context to the router.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1701

## 🛠 Maintenance
## 📚 Documentation
