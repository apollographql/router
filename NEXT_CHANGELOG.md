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
<summary>
  Output of <code>./scripts/public_items.sh</code> for 0.9.6
</summary>
<pre>
use apollo_router::ApolloRouter;
use apollo_router::Configuration;
use apollo_router::ConfigurationKind;
use apollo_router::Context;
use apollo_router::Executable;
use apollo_router::Request;
use apollo_router::Response;
use apollo_router::Schema;
use apollo_router::SchemaKind;
use apollo_router::ShutdownKind;
use apollo_router::error::CacheResolverError;
use apollo_router::error::Error;
use apollo_router::error::FetchError;
use apollo_router::error::GraphQLError;
use apollo_router::error::JsonExtError;
use apollo_router::error::Location;
use apollo_router::error::NewErrorBuilder;
use apollo_router::error::ParseErrors;
use apollo_router::error::PlannerErrors;
use apollo_router::error::QueryPlannerError;
use apollo_router::error::SchemaError;
use apollo_router::error::ServiceBuildError;
use apollo_router::error::SpecError;
use apollo_router::json_ext::Object;
use apollo_router::json_ext::Path;
use apollo_router::json_ext::PathElement;
use apollo_router::layers::ServiceBuilderExt;
use apollo_router::layers::ServiceExt;
use apollo_router::layers::async_checkpoint::AsyncCheckpointLayer;
use apollo_router::layers::async_checkpoint::AsyncCheckpointService;
use apollo_router::layers::cache::CachingLayer;
use apollo_router::layers::cache::CachingService;
use apollo_router::layers::instrument::InstrumentLayer;
use apollo_router::layers::instrument::InstrumentService;
use apollo_router::layers::map_future_with_context::MapFutureWithContextLayer;
use apollo_router::layers::map_future_with_context::MapFutureWithContextService;
use apollo_router::layers::sync_checkpoint::CheckpointLayer;
use apollo_router::layers::sync_checkpoint::CheckpointService;
use apollo_router::main;
use apollo_router::mock_service;
use apollo_router::plugin::DynPlugin;
use apollo_router::plugin::Handler;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginFactory;
use apollo_router::plugin::plugins;
use apollo_router::plugin::register_plugin;
use apollo_router::plugin::serde::deserialize_header_name;
use apollo_router::plugin::serde::deserialize_header_value;
use apollo_router::plugin::serde::deserialize_option_header_name;
use apollo_router::plugin::serde::deserialize_option_header_value;
use apollo_router::plugin::serde::deserialize_regex;
use apollo_router::plugin::test::IntoSchema;
use apollo_router::plugin::test::MockExecutionService;
use apollo_router::plugin::test::MockQueryPlanningService;
use apollo_router::plugin::test::MockRouterService;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::plugin::test::MockSubgraphService;
use apollo_router::plugin::test::NewPluginTestHarnessBuilder;
use apollo_router::plugin::test::PluginTestHarness;
use apollo_router::plugins::csrf::CSRFConfig;
use apollo_router::plugins::csrf::Csrf;
use apollo_router::plugins::rhai::Conf;
use apollo_router::plugins::rhai::Rhai;
use apollo_router::plugins::telemetry::ROUTER_SPAN_NAME;
use apollo_router::plugins::telemetry::Telemetry;
use apollo_router::plugins::telemetry::apollo::Config;
use apollo_router::plugins::telemetry::config::AttributeArray;
use apollo_router::plugins::telemetry::config::AttributeValue;
use apollo_router::plugins::telemetry::config::Conf;
use apollo_router::plugins::telemetry::config::GenericWith;
use apollo_router::plugins::telemetry::config::Metrics;
use apollo_router::plugins::telemetry::config::MetricsCommon;
use apollo_router::plugins::telemetry::config::Propagation;
use apollo_router::plugins::telemetry::config::Sampler;
use apollo_router::plugins::telemetry::config::SamplerOption;
use apollo_router::plugins::telemetry::config::Trace;
use apollo_router::plugins::telemetry::config::Tracing;
use apollo_router::query_planner::OperationKind;
use apollo_router::query_planner::QueryPlan;
use apollo_router::query_planner::QueryPlanOptions;
use apollo_router::register_plugin;
use apollo_router::services::ErrorNewExecutionResponseBuilder;
use apollo_router::services::ErrorNewQueryPlannerResponseBuilder;
use apollo_router::services::ErrorNewRouterResponseBuilder;
use apollo_router::services::ErrorNewSubgraphResponseBuilder;
use apollo_router::services::ExecutionRequest;
use apollo_router::services::ExecutionResponse;
use apollo_router::services::ExecutionService;
use apollo_router::services::FakeNewExecutionRequestBuilder;
use apollo_router::services::FakeNewExecutionResponseBuilder;
use apollo_router::services::FakeNewRouterRequestBuilder;
use apollo_router::services::FakeNewRouterResponseBuilder;
use apollo_router::services::FakeNewSubgraphRequestBuilder;
use apollo_router::services::FakeNewSubgraphResponseBuilder;
use apollo_router::services::NewExecutionRequestBuilder;
use apollo_router::services::NewExecutionResponseBuilder;
use apollo_router::services::NewExecutionServiceBuilder;
use apollo_router::services::NewQueryPlannerRequestBuilder;
use apollo_router::services::NewQueryPlannerResponseBuilder;
use apollo_router::services::NewRouterRequestBuilder;
use apollo_router::services::NewRouterResponseBuilder;
use apollo_router::services::NewRouterServiceBuilder;
use apollo_router::services::NewSubgraphRequestBuilder;
use apollo_router::services::NewSubgraphResponseBuilder;
use apollo_router::services::PluggableRouterServiceBuilder;
use apollo_router::services::QueryPlannerContent;
use apollo_router::services::QueryPlannerRequest;
use apollo_router::services::QueryPlannerResponse;
use apollo_router::services::ResponseBody;
use apollo_router::services::RouterRequest;
use apollo_router::services::RouterResponse;
use apollo_router::services::RouterService;
use apollo_router::services::SubgraphRequest;
use apollo_router::services::SubgraphResponse;
use apollo_router::services::SubgraphService;
use apollo_router::services::http_compat::FakeNewRequestBuilder;
use apollo_router::services::http_compat::IntoHeaderName;
use apollo_router::services::http_compat::IntoHeaderValue;
use apollo_router::services::http_compat::NewRequestBuilder;
use apollo_router::services::http_compat::Request;
use apollo_router::services::http_compat::Response;
use apollo_router::subscriber::RouterSubscriber;
use apollo_router::subscriber::is_global_subscriber_set;
use apollo_router::subscriber::replace_layer;
use apollo_router::subscriber::set_global_subscriber;
</pre>
</details>

By [@SimonSapin](https://github.com/SimonSapin)

### `apollo_router`’s `Error` struct has been renamed to `GraphQLError` ([PR #1302](https://github.com/apollographql/router/pull/1302))

This isn’t actually breaking since `Error` is now a `type` alias,
but it is deprecated so you may see compiler warnings
until you apply the rename in your code.

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

