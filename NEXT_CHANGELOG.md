# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

- **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

  Description! And a link to a [reference](http://url)
-->

# [v0.1.0-preview.7] - (unreleased)
## ❗ BREAKING ❗
### Plugin utilities cleanup ([PR #819](https://github.com/apollographql/router/pull/819))
Utilities around creating Request and Response structures have been migrated to builders.

Migration:
* `plugin_utils::RouterRequest::builder()`->`RouterRequest::fake_builder()`
* `plugin_utils::RouterResponse::builder()`->`RouterResponse::fake_builder()`

In addition, the `plugin_utils` module has been removed. Mock service functionality has been migrated to `plugin::utils::test`.

## 🚀 Features

## 🐛 Fixes

## 🛠 Maintenance

## 📚 Documentation
### Enhanced rust docs ([PR #819](https://github.com/apollographql/router/pull/819))
Many more rust docs have been added.
