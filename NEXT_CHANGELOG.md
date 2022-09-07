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

The Router's Prometheus interface is now exposed at `127.0.0.1:9090/metrics`, rather than `http://0.0.0.0:4000/plugins/apollo.telemetry/prometheus`.  This should be both more secure and also more generally compatible with the default settings that Prometheus expects (which also uses port `9090` and just `/metrics` as its defaults).

To expose to a non-localhost interface, it is necessary to explicitly opt-into binding to a socket address of `0.0.0.0:9090` (i.e., all interfaces on port 9090) or a specific available interface (e.g., `192.168.4.1`) on the host.

Have a look at the Features section to learn how to customize the listen address and the path

## 🚀 Features

### Allow users to customize the prometheus endpoint URL ([#1645](https://github.com/apollographql/router/issues/1645))

You can now customize the prometheus endpoint URL in your yml configuration:

```yml
telemetry:
  metrics:
    prometheus:
      listen: 127.0.0.1:9090 # default
      path: /metrics # default
      enabled: true
```

By [@o0Ignition0o](https://github.com/@o0Ignition0o) in https://github.com/apollographql/router/pull/1654


## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
