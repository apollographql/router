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

# [0.9.6] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗

### Rework the entire public API structure ([PR #1216](https://github.com/apollographql/router/pull/1216),  [PR #1242](https://github.com/apollographql/router/pull/1242),  [PR #1267](https://github.com/apollographql/router/pull/1267),  [PR #1277](https://github.com/apollographql/router/pull/1277))

* Many items have been removed from the public API and made private.
  If you still need some of them, please file an issue.

* Many reexports have been removed, 
  notably from the crate root and all of the `prelude` module.
  Corresponding items need to be imported from another location instead,
  usually the module that define them.

* Some items have moved and need to be imported from a different location.

For example, here are the changes made to `examples/add-timestamp-header/src/main.rs`:

```diff
-use apollo_router::{plugin::utils, Plugin, RouterRequest, RouterResponse};
+use apollo_router::plugin::test;
+use apollo_router::plugin::Plugin;
+use apollo_router::services::{RouterRequest, RouterResponse};
```
```diff
-let mut mock = utils::test::MockRouterService::new();
+let mut mock = test::MockRouterService::new();
```
```diff
-if let apollo_router::ResponseBody::GraphQL(response) =
+if let apollo_router::services::ResponseBody::GraphQL(response) =
     service_response.next_response().await.unwrap()
 {
```

If you’re unsure where a given item needs to be imported from when porting code,
unfold the listing below and use your browser’s search function (CTRL+F or ⌘+F).

<details>
<summary>Listing of paths in the 0.9.6 public API</summary>
<pre>
apollo_router::ApolloRouter
apollo_router::Configuration
apollo_router::ConfigurationKind
apollo_router::Context
apollo_router::Executable
apollo_router::Request
apollo_router::Response
apollo_router::Schema
apollo_router::SchemaKind
apollo_router::ShutdownKind
apollo_router::error::CacheResolverError
apollo_router::error::Error
apollo_router::error::FetchError
apollo_router::error::JsonExtError
apollo_router::error::Location
apollo_router::error::NewErrorBuilder
apollo_router::error::ParseErrors
apollo_router::error::PlannerErrors
apollo_router::error::QueryPlannerError
apollo_router::error::SchemaError
apollo_router::error::ServiceBuildError
apollo_router::error::SpecError
apollo_router::json_ext::Object
apollo_router::json_ext::Path
apollo_router::json_ext::PathElement
apollo_router::layers::ServiceBuilderExt
apollo_router::layers::ServiceExt
apollo_router::layers::async_checkpoint::AsyncCheckpointLayer
apollo_router::layers::async_checkpoint::AsyncCheckpointService
apollo_router::layers::cache::CachingLayer
apollo_router::layers::cache::CachingService
apollo_router::layers::instrument::InstrumentLayer
apollo_router::layers::instrument::InstrumentService
apollo_router::layers::map_future_with_context::MapFutureWithContextLayer
apollo_router::layers::map_future_with_context::MapFutureWithContextService
apollo_router::layers::sync_checkpoint::CheckpointLayer
apollo_router::layers::sync_checkpoint::CheckpointService
apollo_router::main
apollo_router::mock_service
apollo_router::plugin::DynPlugin
apollo_router::plugin::Handler
apollo_router::plugin::Plugin
apollo_router::plugin::PluginFactory
apollo_router::plugin::plugins
apollo_router::plugin::register_plugin
apollo_router::plugin::serde::deserialize_header_name
apollo_router::plugin::serde::deserialize_header_value
apollo_router::plugin::serde::deserialize_option_header_name
apollo_router::plugin::serde::deserialize_option_header_value
apollo_router::plugin::serde::deserialize_regex
apollo_router::plugin::test::IntoSchema
apollo_router::plugin::test::MockExecutionService
apollo_router::plugin::test::MockQueryPlanningService
apollo_router::plugin::test::MockRouterService
apollo_router::plugin::test::MockSubgraph
apollo_router::plugin::test::MockSubgraphService
apollo_router::plugin::test::NewPluginTestHarnessBuilder
apollo_router::plugin::test::PluginTestHarness
apollo_router::plugins::csrf::CSRFConfig
apollo_router::plugins::csrf::Csrf
apollo_router::plugins::rhai::Conf
apollo_router::plugins::rhai::Rhai
apollo_router::plugins::telemetry::ROUTER_SPAN_NAME
apollo_router::plugins::telemetry::Telemetry
apollo_router::plugins::telemetry::apollo::Config
apollo_router::plugins::telemetry::config::AttributeArray
apollo_router::plugins::telemetry::config::AttributeValue
apollo_router::plugins::telemetry::config::Conf
apollo_router::plugins::telemetry::config::GenericWith
apollo_router::plugins::telemetry::config::Metrics
apollo_router::plugins::telemetry::config::MetricsCommon
apollo_router::plugins::telemetry::config::Propagation
apollo_router::plugins::telemetry::config::Sampler
apollo_router::plugins::telemetry::config::SamplerOption
apollo_router::plugins::telemetry::config::Trace
apollo_router::plugins::telemetry::config::Tracing
apollo_router::query_planner::OperationKind
apollo_router::query_planner::QueryPlan
apollo_router::query_planner::QueryPlanOptions
apollo_router::register_plugin
apollo_router::services::ErrorNewExecutionResponseBuilder
apollo_router::services::ErrorNewQueryPlannerResponseBuilder
apollo_router::services::ErrorNewRouterResponseBuilder
apollo_router::services::ErrorNewSubgraphResponseBuilder
apollo_router::services::ExecutionRequest
apollo_router::services::ExecutionResponse
apollo_router::services::ExecutionService
apollo_router::services::FakeNewExecutionRequestBuilder
apollo_router::services::FakeNewExecutionResponseBuilder
apollo_router::services::FakeNewRouterRequestBuilder
apollo_router::services::FakeNewRouterResponseBuilder
apollo_router::services::FakeNewSubgraphRequestBuilder
apollo_router::services::FakeNewSubgraphResponseBuilder
apollo_router::services::NewExecutionRequestBuilder
apollo_router::services::NewExecutionResponseBuilder
apollo_router::services::NewExecutionServiceBuilder
apollo_router::services::NewQueryPlannerRequestBuilder
apollo_router::services::NewQueryPlannerResponseBuilder
apollo_router::services::NewRouterRequestBuilder
apollo_router::services::NewRouterResponseBuilder
apollo_router::services::NewRouterServiceBuilder
apollo_router::services::NewSubgraphRequestBuilder
apollo_router::services::NewSubgraphResponseBuilder
apollo_router::services::PluggableRouterServiceBuilder
apollo_router::services::QueryPlannerContent
apollo_router::services::QueryPlannerRequest
apollo_router::services::QueryPlannerResponse
apollo_router::services::ResponseBody
apollo_router::services::RouterRequest
apollo_router::services::RouterResponse
apollo_router::services::RouterService
apollo_router::services::SubgraphRequest
apollo_router::services::SubgraphResponse
apollo_router::services::SubgraphService
apollo_router::services::http_compat::FakeNewRequestBuilder
apollo_router::services::http_compat::IntoHeaderName
apollo_router::services::http_compat::IntoHeaderValue
apollo_router::services::http_compat::NewRequestBuilder
apollo_router::services::http_compat::Request
apollo_router::services::http_compat::Response
apollo_router::subscriber::RouterSubscriber
apollo_router::subscriber::is_global_subscriber_set
apollo_router::subscriber::replace_layer
apollo_router::subscriber::set_global_subscriber
</pre>

<details>
<summary>Generated with:</summary>
<pre>
cargo +nightly rustdoc --lib -p apollo-router -- \
  -Z unstable-options --output-format json
< target/doc/apollo_router.json > target/public.txt jq -r '
    [
      .paths[] |
      select(.kind != "module" and .kind != "variant") |
      .path |
      select(.[0] == "apollo_router") |
      join("::")
    ] |
    sort |
    .[]
  '
</pre>
</details>
</details>

By [@SimonSapin](https://github.com/SimonSapin)

### Entry point improvements ([PR #1227](https://github.com/apollographql/router/pull/1227)) ([PR #1234](https://github.com/apollographql/router/pull/1234)) ([PR #1239](https://github.com/apollographql/router/pull/1239)) ([PR #1263](https://github.com/apollographql/router/pull/1263))

The interfaces around the entry point have been improved for naming consistency and to enable reuse when customization is required. 
Most users will continue to use:
```rust
apollo_router::main()  
```

However, if you want to specify extra customization to configuration/schema/shutdown then you may use `Executable::builder()` to override behavior. 

```rust
use apollo_router::Executable;
Executable::builder()
  .router_builder_fn(|configuration, schema| ...) // Optional
  .start().await?
```

Migration tips:
* Calls to `ApolloRouterBuilder::default()` should be migrated to `ApolloRouter::builder`.
* `FederatedServerHandle` has been renamed to `ApolloRouterHandle`.
* The ability to supply your own `RouterServiceFactory` has been removed.
* `StateListener`. This made the internal state machine unnecessarily complex. `listen_address()` remains on `ApolloRouterHandle`.
* `FederatedServerHandle::shutdown()` has been removed. Instead, dropping `ApolloRouterHandle` will cause the router to shutdown.
* `FederatedServerHandle::ready()` has been renamed to `FederatedServerHandle::listen_address()`, it will return the address when the router is ready to serve requests.
* `FederatedServerError` has been renamed to `ApolloRouterError`.
* `main_rt` should be migrated to `Executable::builder()`

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1227 https://github.com/apollographql/router/pull/1234 https://github.com/apollographql/router/pull/1239 https://github.com/apollographql/router/pull/1263

### Fixed control flow in helm chart for volume mounts & environment variables ([PR #1283](https://github.com/apollographql/router/issues/1283))

You will now be able to actually use the helm chart without being on a managed graph. 

By [@LockedThread](https://github.com/LockedThread) in https://github.com/apollographql/router/pull/1283


### Deny unknown fields on configuration [PR #1278](https://github.com/apollographql/router/pull/1278)
Do not silently skip some bad configuration, now if you add an unknown configuration field at the root of your configuration file it will return an error.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1278

## 🚀 Features ( :rocket: )

### Add support for modifying variables from a plugin. [PR #1257](https://github.com/apollographql/router/pull/1257)

Previously, it was not possible to modify variables in a `Request` from a plugin. This is now supported in both Rust and Rhai plugins.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1257

## 🐛 Fixes

### Restrict static introspection to only `__schema` and `__type` ([PR #1299](https://github.com/apollographql/router/pull/1299))
Queries with selected field names starting with `__` are recognized as introspection queries. This includes `__schema`, `__type` and `__typename`. However, `__typename` is introspection at query time which is different from `__schema` and `__type` because two of the later can be answered with queries with empty input variables. This change will restrict introspection to only `__schema` and `__type`.

By [@dingxiangfei2009](https://github.com/dingxiangfei2009) in https://github.com/apollographql/router/pull/1299

### Fix scaffold support ([PR #1293](https://github.com/apollographql/router/pull/1293))

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1293

### Support introspection object types ([PR #1240](https://github.com/apollographql/router/pull/1240))

Introspection queries can use a set of object types defined in the specification. The query parsing code was not recognizing them,
resulting in some introspection queries not working.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1240

### Update the scaffold template so it works with streams ([#1247](https://github.com/apollographql/router/issues/1247))

Release v0.9.4 changed the way we deal with Response objects, which can now be streams.
This Pull request updates the scaffold template so it generates plugins that are compatible with the new Plugin API.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1248


### Fix fragment selection on interfaces ([PR #1295](https://github.com/apollographql/router/pull/1295))

Fragments type conditions were not checked correctly on interfaces, resulting in invalid null fields added to the response
or valid data being nullified.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1295

## 🛠 Maintenance ( :hammer_and_wrench: )

### Remove typed-builder ([PR #1218](https://github.com/apollographql/router/pull/1218))
Migrate all typed-builders code to buildstructor
By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1218
## 📚 Documentation ( :books: )

