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

### **Headline** ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [0.10.1] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗
## 🚀 Features

### Add support to add custom resources on metrics. [PR #1354](https://github.com/apollographql/router/pull/1354)

Resources are almost like attributes but there are more globals. They are directly configured on the metrics exporter which means you'll always have these resources on each of your metrics. It could be pretty useful to set
a service name for example to let you more easily find metrics related to a specific service.

```yaml
telemetry:
  metrics:
    common:
      resources:
        # Set the service name to easily find metrics related to the apollo-router in your metrics dashboards
        service.name: "apollo-router"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1354

## 🐛 Fixes

### Fix fragment on interface without typename [PR #1371](https://github.com/apollographql/router/pull/1371)

When the subgraph doesn't return the typename and the type condition of a fragment is an interface, we should return the values if the entity implements the interface

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1371

### Accept nullable list as input [PR #1363](https://github.com/apollographql/router/pull/1363)

Do not throw a validation error when you give `null` for an input variable of type `[Int!]`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1363


## 🛠 Maintenance
## 📚 Documentation
