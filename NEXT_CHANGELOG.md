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

### Aggregate usage reports in streaming and set the timeout to 5 seconds [PR #1066](https://github.com/apollographql/router/pull/1066)
The metrics plugin was allocating chunks of usage reports to aggregate them right after, this was replaced by a streaming loop. The interval for sending the reports to spaceport was reduced from 10s to 5s.

### Put back the ability to use environment variable expansion for telemetry endpoints [PR #1092](https://github.com/apollographql/router/pull/1092)
Adds the ability to use environment variable expansion for the configuration of agent/collector endpoint for Jaeger, OTLP, Datadog.

## 🛠 Maintenance
## 📚 Documentation
### Add CORS documentation ([PR #1044](https://github.com/apollographql/router/pull/1044))
We've updated the CORS documentation to reflect the recent [CORS and CSRF](https://github.com/apollographql/router/pull/1006) updates.

## 🐛 Fixes
