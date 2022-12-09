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

### Protoc now required to build ([Issue #1970](https://github.com/apollographql/router/issues/1970))

Protoc is now required to build Apollo Router. Upgrading to Open Telemetry 0.18 has enabled us to upgrade tonic which in turn no longer bundles protoc.
Users must install it themselves https://grpc.io/docs/protoc-installation/.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1970

### Jaeger scheduled_delay moved to batch_processor->scheduled_delay ([Issue #2232](https://github.com/apollographql/router/issues/2232))

Jager config previously allowed configuration of scheduled_delay for batch span processor. To bring it in line with all other exporters this is now set using a batch_processor section.

Before:
```yaml
telemetry:
  tracing:
    jaeger:
      scheduled_delay: 100ms
```

After:
```yaml
telemetry:
  tracing:
    jaeger:
      batch_processor:
        scheduled_delay: 100ms
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1970

## 🚀 Features
### Tracing batch span processor is now configurable ([Issue #2232](https://github.com/apollographql/router/issues/2232))

Exporting traces often requires performance tuning based on the throughput of the router, sampling settings and ingestion capability of tracing ingress.

All exporters now support configuring the batch span processor in the router yaml. 
```yaml
telemetry:
  apollo:
    batch_processor:
      scheduled_delay: 100ms
      max_concurrent_exports: 1000
      max_export_batch_size: 10000
      max_export_timeout: 100s
      max_queue_size: 10000
  tracing:
    jaeger|zipkin|otlp|datadog:
      batch_processor:
        scheduled_delay: 100ms
        max_concurrent_exports: 1000
        max_export_batch_size: 10000
        max_export_timeout: 100s
        max_queue_size: 10000
```

See the Open Telemetry docs for more information.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1970

### Add support for setting multi-value header keys to rhai ([Issue #2211](https://github.com/apollographql/router/issues/2211))

Adds support for setting a header map key with an array. This causes the HeaderMap key/values to be appended() to the map, rather than inserted().

Example use from rhai as:

```
  response.headers["set-cookie"] = [
    "foo=bar; Domain=localhost; Path=/; Expires=Wed, 04 Jan 2023 17:25:27 GMT; HttpOnly; Secure; SameSite=None",
    "foo2=bar2; Domain=localhost; Path=/; Expires=Wed, 04 Jan 2023 17:25:27 GMT; HttpOnly; Secure; SameSite=None",
  ];
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2219

## 🐛 Fixes

### Filter nullified deferred responses ([Issue #2213](https://github.com/apollographql/router/issues/2168))

[`@defer` spec updates](https://github.com/graphql/graphql-spec/compare/01d7b98f04810c9a9db4c0e53d3c4d54dbf10b82...f58632f496577642221c69809c32dd46b5398bd7#diff-0f02d73330245629f776bb875e5ca2b30978a716732abca136afdd028d5cd33cR448-R470)
mandate that a deferred response should not be sent if its path points to an element of the response that was nullified
in a previous payload.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2184

### wait for opentelemetry tracer provider to shutdown ([PR #2191](https://github.com/apollographql/router/pull/2191))

When we drop Telemetry we spawn a thread to perform the global opentelemetry trace provider shutdown. The documentation of this function indicates that "This will invoke the shutdown method on all span processors. span processors should export remaining spans before return". We should give that process some time to complete (5 seconds currently) before returning from the `drop`. This will provide more opportunity for spans to be exported.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2191

## 🛠 Maintenance

### improve plugin registration predictability ([PR #2181](https://github.com/apollographql/router/pull/2181))

This replaces [ctor](https://crates.io/crates/ctor) with [linkme](https://crates.io/crates/linkme). `ctor` enables rust code to execute before `main`. This can be a source of undefined behaviour and we don't need our code to execute before `main`. `linkme` provides a registration mechanism that is perfect for this use case, so switching to use it makes the router more predictable, simpler to reason about and with a sound basis for future plugin enhancements.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2181

### it_rate_limit_subgraph_requests fixed ([Issue #2213](https://github.com/apollographql/router/issues/2213))

This test was failing frequently due to it being a timing test being run in a single threaded tokio runtime. 

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2218

### Upgrade OpenTelemetry to 0.18 ([Issue #1970](https://github.com/apollographql/router/issues/1970))

Update to OpenTelemetry 0.18.

By [@bryncooke](https://github.com/bryncooke) and [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1970

### Remove spaceport ([Issue #2233](https://github.com/apollographql/router/issues/2233))

Removal significantly simplifies telemetry code and likely to increase performance and reliability.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1970

### Update to Rust 1.65 ([Issue #2220](https://github.com/apollographql/router/issues/2220))

Rust MSRV incremented to 1.65.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2221

## 📚 Documentation
### Create yaml config design guidance ([Issue #2158](https://github.com/apollographql/router/pull/2158))

Added some yaml design guidance to help us create consistent yaml config for new and existing features.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2159
