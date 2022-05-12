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

# [v0.9.0-rc.1] - (unreleased)
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )

### helm chart now supports prometheus metrics [PR #1005](https://github.com/apollographql/router/pull/1005)
The router has supported exporting prometheus metrics for a while. This change updates our helm chart to enable router deployment prometheus metrics. 

Configure by updating your values.yaml or by specifying the value on your helm install command line.

e.g.: helm install --set router.configuration.telemetry.metrics.prometheus.enabled=true <etc...>

Note: prometheus metrics are not enabled by default in the helm chart.

## 🐛 Fixes ( :bug: )

### Configuration for Jaeger/Zipkin agent requires an URL instead of a socket address [PR #1018](https://github.com/apollographql/router/pull/1018)
The router now support URL for a Jaeger or Zipkin agent. So you are able to provide this kind of configuration:
```yaml
telemetry:
  tracing:
    trace_config:
      service_name: router
    jaeger:
      agent:
        endpoint: jaeger:14268
```

### Remove the agent endpoint configuration for Zipkin [PR #1025](https://github.com/apollographql/router/pull/1025)
Zipkin only supports the collector endpoint URL configuration.

## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
### Add documentation for the endpoint configuration in server ([PR #1000](https://github.com/apollographql/router/pull/1000))
Documentation about setting a custom endpoint path for GraphQL queries has been added.

## 🐛 Fixes ( :bug: )

### Fix a panic in Zipkin telemetry configuration [PR #1019](https://github.com/apollographql/router/pull/1019)
Using the reqwest blocking client feature was panicking due to incompatible asynchronous runtime usage.