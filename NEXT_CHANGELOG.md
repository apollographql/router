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

### Remove command line options `--apollo-graph-key` and `--apollo-graph-ref` [PR #1069](https://github.com/apollographql/router/pull/1069)
Using these command lime options exposes sensitive data in the process list. Setting via environment variables is now the only way that these can be set.   
In addition these setting have also been removed from the telemetry configuration in yaml.

## 🚀 Features
## 🐛 Fixes


### Aggregate usage reports in streaming and set the timeout to 5 seconds [PR #1066](https://github.com/apollographql/router/pull/1066)
The metrics plugin was allocating chunks of usagen reports to aggregate them right after, this was replaced by a streaming loop. The interval for sending the reports to spaceport was reduced from 10s to 5s.

## 🛠 Maintenance
## 📚 Documentation

### Document available metrics in Prometheus [PR #1067](https://github.com/apollographql/router/pull/1067)
Add the list of metrics you can have using Prometheus

## 🐛 Fixes
