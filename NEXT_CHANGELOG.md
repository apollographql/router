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

### **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

Description! And a link to a [reference](http://url)
-->

# [0.9.1] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗
## 🚀 Features

### Add an experimental optimization to deduplicate variables in query planner [PR #872](https://github.com/apollographql/router/pull/872)
Get rid of duplicated variables in requests and responses of the query planner. This optimization is disabled by default, if you want to enable it you just need override your configuration:

```yaml title="router.yaml"
server:
  experimental:
    enable_variable_deduplication: true
```

## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
## 🐛 Fixes
