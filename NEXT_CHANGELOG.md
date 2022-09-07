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

### rename originating_request to supergraph_request on various plugin Request structures ([ISSUE #XXXX](https://github.com/apollographql/router/issues/XXXX))

We feel that `supergraph_request` makes it more clear that this is the request received from the client.

By [garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/YYYY

### Allow users to customize the prometheus endpoint URL ([#1645](https://github.com/apollographql/router/issues/1645))

The prometheus endpoint now listens to 0.0.0.0:9090/metrics by default. It previously listened to http://0.0.0.0:4000/plugins/apollo.telemetry/prometheus

Have a look at the Features section to learn how to customize the listen address and the path

By [@o0Ignition0o](https://github.com/@o0Ignition0o) in https://github.com/apollographql/router/pull/1654

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
## 🛠 Maintenance
## 📚 Documentation
