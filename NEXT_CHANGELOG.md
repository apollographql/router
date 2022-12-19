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

# [1.8.0] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Remove timeout from otlp exporter ([Issue #2337](https://github.com/apollographql/router/issues/2337))

`batch_processor` configuration contains timeout, so the existing timeout property has been removed from the parent configuration element.

Before:
```yaml
telemetry:
  tracing:
    otlp:
      timeout: 5s
```
After:
```yaml
telemetry:
  tracing:
    otlp:
      batch_processor:
        timeout: 5s
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2338

## 🚀 Features

### Add support for single instance Redis ([Issue #2300](https://github.com/apollographql/router/issues/2300))

For `experimental_cache` with redis caching it now works with only a single Redis instance if you provide only one URL.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2310

## 🛠 Maintenance

### Simplify telemetry config code ([Issue #2337](https://github.com/apollographql/router/issues/2337))

This brings the telemetry plugin configuration closer to standards recommended in the [yaml design guidance](dev-docs/yaml-design-guidance.md).

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2338
