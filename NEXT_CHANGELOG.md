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

# [0.9.2] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Simplify Context::upsert() [PR #1073](https://github.com/apollographql/router/pull/1073)
Removes the `default` parameter and requires inserted values to implement `Default`.

## 🚀 Features
## 🐛 Fixes

### Put back the ability to use environment variable expansion for telemetry endpoints [PR #1092](https://github.com/apollographql/router/pull/1092)
Adds the ability to use environment variable expansion for the configuration of agent/collector endpoint for Jaeger, OTLP, Datadog.

### Fix the introspection query detection [PR #1100](https://github.com/apollographql/router/pull/1100)
Fix the introspection query detection, for example if you only have `__typename` in the query then it's an introspection query, if it's used with other fields (not prefixed by `__`) then it's not an introspection query.

## 🛠 Maintenance
## 📚 Documentation
### Add CORS documentation ([PR #1044](https://github.com/apollographql/router/pull/1044))
We've updated the CORS documentation to reflect the recent [CORS and CSRF](https://github.com/apollographql/router/pull/1006) updates.

## 🐛 Fixes
