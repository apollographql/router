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

# [v0.1.0-preview.5] - (unreleased)
## ❗ BREAKING ❗
## 🚀 Features

### Install experience [PR #820](https://github.com/apollographql/router/pull/820)

  Added an install script that will automatically download and unzip the router into the local directory.
  For more info see the quickstart documentation.

## 🐛 Fixes

- **Early return a better error when introspection is disabled** [PR #751](https://github.com/apollographql/router/pull/751)

  Instead of returning an error coming from the query planner, we are now returning a proper error explaining that the introspection has been disabled.

## 🛠 Maintenance

- **Switch web server framework from `warp` to `axum`** [PR #751](https://github.com/apollographql/router/pull/751)

  The router is now running by default with an [axum](https://github.com/tokio-rs/axum/) web server instead of `warp`.
  
## 📚 Documentation
