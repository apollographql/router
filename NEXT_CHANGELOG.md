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

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗

The prometheus endpoint now listens to 0.0.0.0:9090/metrics by default. It previously listened to http://0.0.0.0:4000/plugins/apollo.telemetry/prometheus

Have a look at the Features section to learn how to customize the listen address and the path

## 🚀 Features

### Allow users to customize the prometheus endpoint URL ([#1645](https://github.com/apollographql/router/issues/1645))

You can now customize the prometheus endpoint URL in your yml configuration:

```yml
telemetry:
  metrics:
    prometheus:
      listen: 0.0.0.0:9090 # default
      path: /metrics # default
      enabled: true
```

By [@o0Ignition0o](https://github.com/@o0Ignition0o) in https://github.com/apollographql/router/pull/1654


## 🐛 Fixes

### Numerous fixes to preview `@defer` query planning ([Issue #1698](https://github.com/apollographql/router/issues/1698))

Updated to [Federation `2.1.2-alpha.0`](https://github.com/apollographql/federation/pull/2132) which brings in a number of fixes for the preview `@defer` support.  These fixes include:

 - [Empty selection set produced with @defer'd query `federation#2123`](https://github.com/apollographql/federation/issues/2123)
 - [Include directive with operation argument errors out in Fed 2.1 `federation#2124`](https://github.com/apollographql/federation/issues/2124)
 - [query plan sequencing affected with __typename in fragment `federation#2128`](https://github.com/apollographql/federation/issues/2128)
 - [Router Returns Error if __typename Omitted `router#1668`](https://github.com/apollographql/router/issues/1668)

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1711

## 🛠 Maintenance
## 📚 Documentation
