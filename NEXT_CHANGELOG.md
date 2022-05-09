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

- **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

  Description! And a link to a [reference](http://url)
-->

# [v0.1.0-preview.8] - (unreleased)
## ❗ BREAKING ❗

### Add configuration to declare your own GraphQL endpoint [PR #976](https://github.com/apollographql/router/pull/976)
You are now able to declare your own GraphQL endpoint in the config like this:
```yaml
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
  # Default is /
  endpoint: /graphql
```
But we also deleted the `/graphql` endpoint by default, you will know have only one existing GraphQL endpoint and by default it's `/`. If you need to use `/graphql` instead then refer to my previous example.

### rhai scripts should be able to do more things (like rust plugins) [PR #971](https://github.com/apollographql/router/pull/971)
This is a re-working of our rhai scripting support. The intent is to make writing a rhai plugin more like writing a rust plugin, with full participation in the service plugin lifecycle. The work is still some way from complete, but does provide new capabilities (such as logging from rhai) and provides a more solid basis on which we can evolve our implementation. The examples and documentation should make clear how to modify any existing scripts to accomodate the changes.

## 🚀 Features ( :rocket: )

### Apollo studio Usage Reporting [PR #898](https://github.com/apollographql/router/pull/898)
If you have [enabled telemetry](https://www.apollographql.com/docs/router/configuration/apollo-telemetry#enabling-usage-reporting), you can now see field usage reporting for your queries by heading to the Apollo studio fields section.
Here is more information on how to [set up telemetry](https://www.apollographql.com/docs/studio/metrics/usage-reporting#pushing-metrics-from-apollo-server) and [Field Usage](https://www.apollographql.com/docs/studio/metrics/field-usage)

### PluginTestHarness [PR #898](https://github.com/apollographql/router/pull/898)
Added a simple plugin test harness that can provide canned responses to queries. This harness is early in development and the functionality and APIs will probably change. 
```rust
 let mut test_harness = PluginTestHarness::builder()
            .plugin(plugin)
            .schema(Canned)
            .build()
            .await?;

let _ = test_harness
    .call(
        RouterRequest::fake_builder()
            .header("name_header", "test_client")
            .header("version_header", "1.0-test")
            .query(query)
            .and_operation_name(operation_name)
            .and_context(context)
            .build()?,
    )
    .await;
```
## 🐛 Fixes ( :bug: )

### Improve the configuration error report [PR #963](https://github.com/apollographql/router/pull/963)
In case you have unknown properties on your configuration it will highlight the entity with unknown properties. Before we always pointed on the first field of this entity even if it wasn't the bad one, it's now fixed.

### Fix incorrectly omitting content of interface's fragment [PR #949](https://github.com/apollographql/router/pull/949)
Router now distinguish between fragment on concrete type and interface.
If interface is encountered and  `__typename` is queried, additionally checks that returned type implements interface.

### Set the service name if not specified in config or environment [PR #960](https://github.com/apollographql/router/pull/960)
The router now sets "router" as default service name in Opentelemetry traces, that can be replaced using the configuration file or environment variables. It also sets the key "process.executable_name".

### Accept an endpoint URL without scheme for telemetry [PR #964](https://github.com/apollographql/router/pull/964)

Endpoint configuration for Datadog and OTLP take a URL as argument, but was incorrectly recognizing addresses of the format "host:port"

## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
