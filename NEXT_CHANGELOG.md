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

# [v0.1.0-preview.6] - (unreleased)
## ❗ BREAKING ❗

## 🚀 Features

## 🐛 Fixes
### Correctly flag incoming POST requests [#865](https://github.com/apollographql/router/issues/865)
A regression happened during our recent switch to axum that would propagate incoming POST requests as GET requests. This has been fixed and we now have several regression tests, pending more integration tests.
## 🛠 Maintenance

## 📚 Documentation
