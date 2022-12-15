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
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes

### Return an error on duplicate keys in configuration ([Issue #1428](https://github.com/apollographql/router/issues/1428))

If you have duplicated keys in your yaml configuration like this:

```yaml
telemetry:
  tracing:
    propagation:
      jaeger: true
  tracing:
    propagation:
      jaeger: false
```

It will now throw an error on router startup:

`ERROR duplicated keys detected in your yaml configuration: 'telemetry.tracing'`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2270

## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental
