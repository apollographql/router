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

### Remove the agent endpoint configuration for Zipkin [PR #1025](https://github.com/apollographql/router/pull/1025)
Zipkin only supports the collector endpoint URL configuration.

The Zipkin configuration changes from:

```yaml
telemetry:
  tracing:
    trace_config:
      service_name: router
    zipkin:
      collector:
        endpoint: default
```

to:

```yaml
telemetry:
  tracing:
    trace_config:
      service_name: router
    zipkin:
      endpoint: default
```

### CSRF Protection is enabled by default [PR #1006](https://github.com/apollographql/router/pull/1006)
A [Cross-Site Request Forgery protection plugin](https://developer.mozilla.org/en-US/docs/Glossary/CSRF) is enabled by default.

This means [simple requests](https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS#simple_requests) will be rejected from now on (they represent a security risk).

The plugin can be customized as explained in the [CORS and CSRF example](https://github.com/apollographql/router/tree/main/examples/cors-and-csrf/custom-headers.router.yaml)

### CORS default behavior update [PR #1006](https://github.com/apollographql/router/pull/1006)
The CORS allow_headers default behavior changes from:
  - allow only `Content-Type`, `apollographql-client-name` and `apollographql-client-version`
to:
  - mirror the received `access-control-request-headers`

This change loosens the CORS related headers restrictions, so it shouldn't have any impact on your setup.

## 🚀 Features ( :rocket: )

### CSRF Protection [PR #1006](https://github.com/apollographql/router/pull/1006)
The router now embeds a CSRF protection plugin, which is enabled by default. Have a look at the [CORS and CSRF example](https://github.com/apollographql/router/tree/main/examples/cors-and-csrf/custom-headers.router.yaml) to learn how to customize it. [Documentation](https://www.apollographql.com/docs/router/configuration/cors/) will be updated soon!

### Helm chart now supports prometheus metrics [PR #1005](https://github.com/apollographql/router/pull/1005)
The router has supported exporting prometheus metrics for a while. This change updates our helm chart to enable router deployment prometheus metrics. 

Configure by updating your values.yaml or by specifying the value on your helm install command line.

e.g.: helm install --set router.configuration.telemetry.metrics.prometheus.enabled=true <etc...>

Note: prometheus metrics are not enabled by default in the helm chart.

## 🐛 Fixes ( :bug: )

### Delete the requirement of jq command in our install script [PR #1034](https://github.com/apollographql/router/pull/1034)
We're now using `cut` command instead of `jq` which is more standard.

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
### Fix a panic in Zipkin telemetry configuration [PR #1019](https://github.com/apollographql/router/pull/1019)
Using the reqwest blocking client feature was panicking due to incompatible asynchronous runtime usage.

### Improvements to Studio reporting [PR #1020](https://github.com/apollographql/router/pull/1020), [PR #1037](https://github.com/apollographql/router/pull/1037)
Studio reporting now aggregates at the Router only. This architectural change allows us to move towards full reporting functionality.

### Field usage reporting is now reported against the correct schema [PR #1043](https://github.com/apollographql/router/pull/1043)
Previously this was empty, new code will uses API schema hash.

### Add message to logs when Apollo usage reporting is enabled [PR #1029](https://github.com/apollographql/router/pull/1029)
When studio reporting is enabled the user is notified in the router logs that data is sent to Apollo.

### Check that an object's `__typename` is part of the schema [PR #1033](https://github.com/apollographql/router/pull/1033)
In case a subgraph returns an object with a `__typename` field referring to a type that is not in the API schema, as with usage of the `@inaccessible` directive on object types, the whole object should be replaced with a `null`.

## 🛠 Maintenance ( :hammer_and_wrench: )

### OpenTracing examples [PR #1015](https://github.com/apollographql/router/pull/1015)
We now have complete examples of OpenTracing usage with Datadog, Jaeger and Zipkin, that can be started with docker-compose.

## 📚 Documentation ( :books: )
### Add documentation for the endpoint configuration in server ([PR #1000](https://github.com/apollographql/router/pull/1000))
Documentation about setting a custom endpoint path for GraphQL queries has been added.
