# Changelog

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

# [0.12.0] - 2022-08-18

## ❗ BREAKING ❗

### Move `experimental.rhai` out of `experimental` ([PR #1365](https://github.com/apollographql/router/pull/1365))

You will need to update your YAML configuration file to use the correct name for `rhai` plugin.

```diff
- plugins:
-   experimental.rhai:
-     filename: /path/to/myfile.rhai
+ rhai:
+   scripts: /path/to/directory/containing/all/my/rhai/scripts (./scripts by default)
+   main: <name of main script to execute> (main.rhai by default)
```

You can now modularise your rhai code. Rather than specifying a path to a filename containing your rhai code, the rhai plugin will now attempt to execute the script specified via `main`. If modules are imported, the rhai plugin will search for those modules in the `scripts` directory. for more details about how rhai makes use of modules, look at [the rhai documentation](https://rhai.rs/book/ref/modules/import.html).

The simplest migration will be to set `scripts` to the directory containing your `myfile.rhai` and to rename your `myfile.rhai` to `main.rhai`.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1365

## 🐛 Fixes

### The opentelemetry-otlp crate needs a http-client feature ([PR #1392](https://github.com/apollographql/router/pull/1392))

The opentelemetry-otlp crate only checks at runtime if a HTTP client was added through
cargo features. We now use reqwest for that.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/1392

### Expose the custom endpoints from RouterServiceFactory ([PR #1402](https://github.com/apollographql/router/pull/1402))

Plugin HTTP endpoints registration was broken during the Tower refactoring. We now make sure that the list
of endpoints is generated from the `RouterServiceFactory` instance.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/1402

## 🛠 Maintenance

### Dependency updates ([PR #1389](https://github.com/apollographql/router/issues/1389), [PR #1394](https://github.com/apollographql/router/issues/1394), [PR #1395](https://github.com/apollographql/router/issues/1395))

Dependency updates were blocked for some time due to incompatibilities:

- #1389: the router-bridge crate needed a new version of `deno_core` in its workspace that would not fix the version of `once_cell`. Now that it is done we can update `once_cell` in the router
- #1395: `clap` at version 3.2 changed the way values are extracted from matched arguments, which resulted in panics. This is now fixed and we can update `clap` in the router and related crates
- #1394: broader dependency updates now that everything is locked
- #1410: revert tracing update that caused two telemetry tests to fail (the router binary is not affected)

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1389 https://github.com/apollographql/router/pull/1394 https://github.com/apollographql/router/pull/1395 and [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1410

# [0.11.0] - 2022-07-12

## ❗ BREAKING ❗

### Relax plugin api mutability ([PR #1340](https://github.com/apollographql/router/pull/1340) ([PR #1289](https://github.com/apollographql/router/pull/1289))

the `Plugin::*_service()` methods were taking a `&mut self` as argument, but since
they work like a tower Layer, they can use `&self` instead. This change
then allows us to move from Buffer to service factories for the query
planner, execution and subgraph services.

**Services are now created on the fly at session creation, so if any state must be shared
between executions, it should be stored in an `Arc<Mutex<_>>` in the plugin and cloned
into the new service in the `Plugin::*_service()` methods**.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1340 https://github.com/apollographql/router/pull/1289

## 🚀 Features

### Add support to add custom resources on metrics. ([PR #1354](https://github.com/apollographql/router/pull/1354))

Resources are almost like attributes but more global. They are directly configured on the metrics exporter which means you'll always have these resources on each of your metrics.  This functionality can be used to, for example,
apply a `service.name` to metrics to make them easier to find in larger infrastructure, as demonstrated here:

```yaml
telemetry:
  metrics:
    common:
      resources:
        # Set the service name to easily find metrics related to the apollo-router in your metrics dashboards
        service.name: "apollo-router"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1354

## 🐛 Fixes

### Fix fragment on interface without typename ([PR #1371](https://github.com/apollographql/router/pull/1371))

When the subgraph doesn't return the `__typename` and the type condition of a fragment is an interface, we should return the values if the entity implements the interface

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1371

### Fix detection of an introspection query ([PR #1370](https://github.com/apollographql/router/pull/1370))

A query that only contains `__typename` at the root will now special-cased as merely an introspection query and will bypass more complex query-planner execution (its value will just be `Query`).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1370

### Accept nullable list as input ([PR #1363](https://github.com/apollographql/router/pull/1363))

Do not throw a validation error when you give `null` for an input variable of type `[Int!]`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1363

## 🛠 Maintenance

### Replace Buffers of tower services with service factories ([PR #1289](https://github.com/apollographql/router/pull/1289) [PR #1355](https://github.com/apollographql/router/pull/1355))

Tower services should be used by creating a new service instance for each new session
instead of going through a `Buffer`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1289  https://github.com/apollographql/router/pull/1355

### Execute the query plan's first response directly ([PR #1357](https://github.com/apollographql/router/issues/1357))

The query plan was previously executed in a spawned task to prepare for the `@defer` implementation, but we can actually
generate the first response right inside the same future.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1357

### Remove deprecated `failure` crate from the dependency tree ([PR #1373](https://github.com/apollographql/router/pull/1373))

This should fix automated reports about [GHSA-jq66-xh47-j9f3](https://github.com/advisories/GHSA-jq66-xh47-j9f3).

By [@yanns](https://github.com/yanns) in https://github.com/apollographql/router/pull/1373

### Render embedded Sandbox instead of landing page ([PR #1369](https://github.com/apollographql/router/pull/1369))

Open the router URL in a browser and start querying the router from the Apollo Sandbox.

By [@mayakoneval](https://github.com/mayakoneval) in https://github.com/apollographql/router/pull/1369

## 📚 Documentation

### Various documentation edits ([PR #1329](https://github.com/apollographql/router/issues/1329))

By [@StephenBarlow](https://github.com/StephenBarlow) in https://github.com/apollographql/router/pull/1329


# [0.10.0] - 2022-07-05

## ❗ BREAKING ❗

### Change configuration for custom attributes for metrics in telemetry plugin ([PR #1300](https://github.com/apollographql/router/pull/1300)

To create a distinction between subgraph metrics and router metrics, a distiction has been made in the configuration.  Therefore, a new configuration section called `router` has been introduced and Router-specific properties are now listed there, as seen here:

```diff
telemetry:
  metrics:
    common:
      attributes:
-        static:
-          - name: "version"
-            value: "v1.0.0"
-        from_headers:
-          - named: "content-type"
-            rename: "payload_type"
-            default: "application/json"
-          - named: "x-custom-header-to-add"
+        router:
+          static:
+            - name: "version"
+              value: "v1.0.0"
+          request:
+            header:
+              - named: "content-type"
+                rename: "payload_type"
+                default: "application/json"
+              - named: "x-custom-header-to-add"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1300

### Rename `http_compat` to `http_ext` ([PR #1291](https://github.com/apollographql/router/pull/1291))

The module provides extensions to the `http` crate which are specific to the way we use that crate in the router. This change also cleans up the provided extensions and fixes a few potential sources of error (by removing them)
such as the `Request::mock()` function.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1291

### Rework the entire public API structure ([PR #1216](https://github.com/apollographql/router/pull/1216),  [PR #1242](https://github.com/apollographql/router/pull/1242),  [PR #1267](https://github.com/apollographql/router/pull/1267),  [PR #1277](https://github.com/apollographql/router/pull/1277), [PR #1303](https://github.com/apollographql/router/pull/1303))

* Many items have been removed from the public API and made private.
  If you were relying on these previously-public methods and find that they are no longer available, please open an issue with your use case so we can consider how we want to re-introduce them.

* Many re-exports have been removed.
  Most notably from the crate root and all of the `prelude` modules.
  These items now need to be imported from another location instead,
  most often the module that defines them.

* Some items have moved and need to be imported from a new location.

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

If you're unsure where a given item needs to be imported from when porting code,
unfold the listing below and use your browser's search function (CTRL+F or ⌘+F).

<details>
<summary>
  Output of <code>./scripts/public_items.sh</code> for 0.10.0
</summary>
<pre>
use apollo_router::ApolloRouter;
use apollo_router::Configuration;
use apollo_router::ConfigurationKind;
use apollo_router::Context;
use apollo_router::Error;
use apollo_router::Executable;
use apollo_router::Request;
use apollo_router::Response;
use apollo_router::Schema;
use apollo_router::SchemaKind;
use apollo_router::ShutdownKind;
use apollo_router::error::CacheResolverError;
use apollo_router::error::FetchError;
use apollo_router::error::JsonExtError;
use apollo_router::error::Location;
use apollo_router::error::ParseErrors;
use apollo_router::error::PlannerErrors;
use apollo_router::error::QueryPlannerError;
use apollo_router::error::SchemaError;
use apollo_router::error::ServiceBuildError;
use apollo_router::error::SpecError;
use apollo_router::graphql::Error;
use apollo_router::graphql::NewErrorBuilder;
use apollo_router::graphql::Request;
use apollo_router::graphql::Response;
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
use apollo_router::services::http_ext::FakeNewRequestBuilder;
use apollo_router::services::http_ext::IntoHeaderName;
use apollo_router::services::http_ext::IntoHeaderValue;
use apollo_router::services::http_ext::NewRequestBuilder;
use apollo_router::services::http_ext::Request;
use apollo_router::services::http_ext::Response;
use apollo_router::subscriber::RouterSubscriber;
use apollo_router::subscriber::is_global_subscriber_set;
use apollo_router::subscriber::replace_layer;
use apollo_router::subscriber::set_global_subscriber;
</pre>
</details>

By [@SimonSapin](https://github.com/SimonSapin)

### Entry point improvements ([PR #1227](https://github.com/apollographql/router/pull/1227)) ([PR #1234](https://github.com/apollographql/router/pull/1234)) ([PR #1239](https://github.com/apollographql/router/pull/1239)), [PR #1263](https://github.com/apollographql/router/pull/1263))

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

### Non-GraphQL response body variants removed from `RouterResponse` ([PR #1307](https://github.com/apollographql/router/pull/1307), [PR #1328](https://github.com/apollographql/router/pull/1328))

The `ResponseBody` enum has been removed.
It had variants for GraphQL and non-GraphQL responses.

It was used:

* In `RouterResponse` which now uses `apollo_router::graphql::Response` instead
* In `Handler` for plugin custom endpoints which now uses `bytes::Bytes` instead

Various type signatures will need changes such as:

```diff
- RouterResponse<BoxStream<'static, ResponseBody>>
+ RouterResponse<BoxStream<'static, graphql::Response>>
```

Necessary code changes might look like:

```diff
- return ResponseBody::GraphQL(response);
+ return response;
```
```diff
- if let ResponseBody::GraphQL(graphql_response) = res {
-     assert_eq!(&graphql_response.errors[0], expected_error);
- } else {
-     panic!("expected a graphql response");
- }
+ assert_eq!(&res.errors[0], expected_error);
```

By [@SimonSapin](https://github.com/SimonSapin)

### Fixed control flow in helm chart for volume mounts & environment variables ([PR #1283](https://github.com/apollographql/router/issues/1283))

You will now be able to actually use the helm chart without being on a managed graph.

By [@LockedThread](https://github.com/LockedThread) in https://github.com/apollographql/router/pull/1283

### Fail when unknown fields are encountered in configuration ([PR #1278](https://github.com/apollographql/router/pull/1278))

Now if you add an unknown configuration field at the root of your configuration file it will return an error, rather than silently continuing with un-recognized options.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1278

## 🚀 Features

### Allow custom subgraph-specific attributes to be added to emitted metrics ([PR #1300](https://github.com/apollographql/router/pull/1300))

Previously, it was only possible to add custom attributes from headers which the router received from the external GraphQL client. Now, you are able to add custom attributes coming from both the headers and the body of either the Router's or the Subgraph's router request or response. You also have the ability to add an attributes from the context. For example:

```yaml
telemetry:
  metrics:
    common:
      attributes:
        router:
          static:
            - name: "version"
              value: "v1.0.0"
          request:
            header:
              - named: "content-type"
                rename: "payload_type"
                default: "application/json"
              - named: "x-custom-header-to-add"
          response:
            body:
              # Take element from the Router's JSON response body router located at a specific path
              - path: .errors[0].extensions.status
                name: error_from_body
          context:
            # Take element from the context within plugin chains and add it in attributes
            - named: my_key
        subgraph:
          all:
            static:
              # Always insert this static value on all metrics for ALL Subgraphs
              - name: kind
                value: subgraph_request
          subgraphs:
            # Apply these only for the SPECIFIC subgraph named `my_subgraph_name`
            my_subgraph_name:
              request:
                header:
                  - named: "x-custom-header"
                body:
                  # Take element from the request body of the router located at this path (here it's the query)
                  - path: .query
                    name: query
                    default: UNKNOWN
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1300

### Add support for modifying variables from a plugin ([PR #1257](https://github.com/apollographql/router/pull/1257))

Previously, it was not possible to modify variables in a `Request` from a plugin. This is now supported via both Rust and Rhai plugins.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1257

## 🐛 Fixes

### Extend fix for compression support to include the DIY Dockerfiles ([PR #1352](https://github.com/apollographql/router/pull/1352))

Compression support is now shown in the DIY Dockerfiles, as a followup to [PR #1279](https://github.com/apollographql/router/pull/1279).

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1352

### Improve URL parsing in endpoint configuration ([PR #1341](https://github.com/apollographql/router/pull/1341))

Specifying an endpoint in this form '127.0.0.1:431' resulted in an error: 'relative URL without a base'. The fix enhances the URL parsing logic to check for these errors and re-parses with a default scheme 'http://' so that parsing succeeds.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1341

### Improve configuration validation and environment expansion ([PR #1331](https://github.com/apollographql/router/pull/1331))

Environment expansion now covers the entire configuration file, and supports non-string types.

This means that it is now possible to use environment variables in the `server` section of the YAML configuration, including numeric and boolean fields.

Environment variables will always be shown in their original form within error messages to prevent leakage of secrets.

These changes allow more of the configuration file to be validated via JSON-schema, as previously we just skipped errors where fields contained environment variables.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1331

### Fix input coercion for a list ([PR #1327](https://github.com/apollographql/router/pull/1327))

The router is now following coercion rules for lists in accordance with [the GraphQL specification](https://spec.graphql.org/June2018/#sec-Type-System.List). In particular, this fixes the case when an input type of `[Int]` with only `1` provided as a value will now be properly coerced to `[1]`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1327

### Returns HTTP 400 Bad Request, rather than a 500, when hitting a query planning error ([PR #1321](https://github.com/apollographql/router/pull/1321))

A query planning error cannot be retried, so this error code more correctly matches the failure mode and indicates to the client that it should not be retried without changing the request.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1321

### Re-enable the subgraph error-redaction functionality ([PR #1317](https://github.com/apollographql/router/pull/1317))

In a re-factoring the `include_subgraph_errors` plugin was disabled. This meant that subgraph error handling was not working as intended. This change re-enables it and improves the functionality with additional logging. As part of the fix, the plugin initialization mechanism was improved to ensure that plugins start in the required sequence.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1317

### Restrict static introspection to only `__schema` and `__type` ([PR #1299](https://github.com/apollographql/router/pull/1299))
Queries with selected field names starting with `__` are recognized as introspection queries. This includes `__schema`, `__type` and `__typename`. However, `__typename` is introspection at query time which is different from `__schema` and `__type` because two of the later can be answered with queries with empty input variables. This change will restrict introspection to only `__schema` and `__type`.

By [@dingxiangfei2009](https://github.com/dingxiangfei2009) in https://github.com/apollographql/router/pull/1299

### Fix plugin scaffolding support ([PR #1293](https://github.com/apollographql/router/pull/1293))

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1293

### Support introspection object types ([PR #1240](https://github.com/apollographql/router/pull/1240))

Introspection queries can use a set of object types defined in the specification. The query parsing code was not recognizing them, resulting in some introspection queries not working.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1240

### Update the scaffold template so it works with streams ([#1247](https://github.com/apollographql/router/issues/1247))

Release v0.9.4 changed the way we deal with `Response` objects, which can now be streams. The scaffold template now generates plugins that are compatible with this new Plugin API.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1248

### Fix fragment selection on interfaces ([PR #1295](https://github.com/apollographql/router/pull/1295))

Fragments type conditions were not being checked correctly on interfaces, resulting in invalid null fields added to the response or valid data being incorrectly `null`-ified.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1295

### Fix fragment selection on queries ([PR #1296](https://github.com/apollographql/router/pull/1296))

The schema object can specify objects for queries, mutations or subscriptions that are not named `Query`, `Mutation` or `Subscription`. Response formatting now supports it.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1296

### Fix fragment selection on unions ([PR #1346](https://github.com/apollographql/router/pull/1346))

Fragments type conditions were not checked correctly on unions, resulting in data being absent.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1346

### Reduce `poll_ready` calls in query deduplication ([PR #1350](https://github.com/apollographql/router/pull/1350))

The query deduplication service was making assumptions on the underlying service's behaviour, which could result in subgraph services panicking.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1350

## 🛠 Maintenance

### chore: Run scaffold tests in CI and xtask only ([PR #1345](https://github.com/apollographql/router/pull/1345))

Run the scaffold tests in CI and through xtask, to keep a steady feedback loop while developping against `cargo test`.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1345

### Update rhai to latest release (1.8.0)  ([PR #1337](https://github.com/apollographql/router/pull/1337)

We had been depending on a pinned git version which had a fix we required. This now updates to the latest release which includes the fix upstream.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1337

### Remove typed-builder ([PR #1218](https://github.com/apollographql/router/pull/1218))
Migrate all typed-builders code to `buildstructor`.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1218

# [0.9.5] - 2022-06-17
## ❗ BREAKING ❗

### Move `experimental.traffic_shaping` out of `experimental` [PR #1229](https://github.com/apollographql/router/pull/1229)
You will need to update your YAML configuration file to use the correct name for `traffic_shaping` plugin.

```diff
- plugins:
-   experimental.traffic_shaping:
-     variables_deduplication: true # Enable the variables deduplication optimization
-     all:
-       query_deduplication: true # Enable query deduplication for all subgraphs.
-     subgraphs:
-       products:
-         query_deduplication: false # Disable query deduplication for products.
+ traffic_shaping:
+   variables_deduplication: true # Enable the variables deduplication optimization
+   all:
+     query_deduplication: true # Enable query deduplication for all subgraphs.
+   subgraphs:
+     products:
+       query_deduplication: false # Disable query deduplication for products.
```

### Rhai plugin `request.sub_headers` renamed to `request.subgraph.headers` [PR #1261](https://github.com/apollographql/router/pull/1261)

Rhai scripts previously supported the `request.sub_headers` attribute so that subgraph request headers could be
accessed. This is now replaced with an extended interface for subgraph requests:

```
request.subgraph.headers
request.subgraph.body.query
request.subgraph.body.operation_name
request.subgraph.body.variables
request.subgraph.body.extensions
request.subgraph.uri.host
request.subgraph.uri.path
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1261

## 🚀 Features

### Add support of compression [PR #1229](https://github.com/apollographql/router/pull/1229)
Add support of request and response compression for the router and all subgraphs. The router is now able to handle `Content-Encoding` and `Accept-Encoding` headers properly. Supported algorithms are `gzip`, `br`, `deflate`.
You can also enable compression on subgraphs requests and responses by updating the `traffic_shaping` configuration:

```yaml
traffic_shaping:
  all:
    compression: br # Enable brotli compression for all subgraphs
  subgraphs:
    products:
      compression: gzip # Enable gzip compression only for subgraph products
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1229

### Add support of multiple uplink URLs [PR #1210](https://github.com/apollographql/router/pull/1210)
Add support of multiple uplink URLs with a comma-separated list in `APOLLO_UPLINK_ENDPOINTS` and for `--apollo-uplink-endpoints`

Example:
```bash
export APOLLO_UPLINK_ENDPOINTS="https://aws.uplink.api.apollographql.com/, https://uplink.api.apollographql.com/"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1210

### Add support for adding extra environment variables and volumes to helm chart [PR #1245](https://github.com/apollographql/router/pull/1245)
You can mount your `supergraph.yaml` into the helm deployment via configmap. Using [Kustomize](https://kustomize.io/) to generate your configmap from your supergraph.yaml is suggested.

Example configmap.yaml snippet:
```yaml
supergraph.yaml:
    server:
        listen: 0.0.0.0:80
```

Example helm config:
```yaml
extraEnvVars:
  - name: APOLLO_ROUTER_SUPERGRAPH_PATH
    value: /etc/apollo/supergraph.yaml
    # sets router log level to debug
  - name: APOLLO_ROUTER_LOG
    value: debug
extraEnvVarsCM: ''
extraEnvVarsSecret: ''

extraVolumes:
  - name: supergraph-volume
    configMap:
      name: some-configmap 
extraVolumeMounts: 
  - name: supergraph-volume
    mountPath: /etc/apollo
```

By [@LockedThread](https://github.com/LockedThread) in https://github.com/apollographql/router/pull/1245

## 🐛 Fixes

### Support introspection object types ([PR #1240](https://github.com/apollographql/router/pull/1240))
Introspection queries can use a set of object types defined in the specification. The query parsing code was not recognizing them,
resulting in some introspection queries not working.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1240

### Update the scaffold template so that it works with streams ([#1247](https://github.com/apollographql/router/issues/1247))
Release v0.9.4 changed the way we deal with `Response` objects, which can now be streams.
The scaffold template has been updated so that it generates plugins that are compatible with the new Plugin API.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1248

### Create the `ExecutionResponse` after the primary response was generated ([PR #1260](https://github.com/apollographql/router/pull/1260))
The `@defer` preliminary work had a surprising side effect: when using methods like `RouterResponse::map_response`, they were
executed before the subgraph responses were received, because they work on the stream of responses.
This PR goes back to the previous behaviour by awaiting the primary response before creating the `ExecutionResponse`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1260

### Use the API schema to generate selections ([PR #1255](https://github.com/apollographql/router/pull/1255))
When parsing the schema to generate selections for response formatting, we should use the API schema instead of the supergraph schema.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1255

## 📚 Documentation

### Update README link to the configuration file  ([PR #1208](https://github.com/apollographql/router/pull/1208))
As the structure of the documentation has changed, the link should point to the `YAML config file` section of the overview.

By [@gscheibel](https://github.com/gscheibel in https://github.com/apollographql/router/pull/1208



# [0.9.4] - 2022-06-14

## ❗ BREAKING ❗


### Groundwork for `@defer` support ([PR #1175](https://github.com/apollographql/router/pull/1175)[PR #1206](https://github.com/apollographql/router/pull/1206))
To prepare for the implementation of the `@defer` directive, the `ExecutionResponse`  and `RouterResponse` types now carry a stream of responses instead of a unique response. For now that stream contains only one item, so there is no change in behaviour. However, the Plugin trait changed to accomodate this, so a couple of steps are required to migrate your plugin so that it is compatible with versions of the router >= v0.9.4:

- Add a dependency to futures in your Cargo.toml:

```diff
+futures = "0.3.21"
```

- Import `BoxStream`, and if your Plugin defines a `router_service` behavior, import `ResponseBody`:

```diff
+ use futures::stream::BoxStream;
+ use apollo_router::ResponseBody;
```

- Update the `router_service` and the `execution_service` sections of your Plugin (if applicable):

```diff
      fn router_service(
         &mut self,
-        service: BoxService<RouterRequest, RouterResponse, BoxError>,
-    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
+        service: BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError>,
+    ) -> BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError> {

[...]

     fn execution_service(
         &mut self,
-        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
-    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
+        service: BoxService<ExecutionRequest, ExecutionResponse<BoxStream<'static, Response>>, BoxError>,
+    ) -> BoxService<ExecutionRequest, ExecutionResponse<BoxStream<'static, Response>>, BoxError> {
```

We can now update our unit tests so they work on a stream of responses instead of a single one:

```diff
         // Send a request
-        let result = test_harness.call_canned().await?;
-        if let ResponseBody::GraphQL(graphql) = result.response.body() {
+        let mut result = test_harness.call_canned().await?;
+
+        let first_response = result
+            .next_response()
+            .await
+            .expect("couldn't get primary response");
+
+        if let ResponseBody::GraphQL(graphql) = first_response {
             assert!(graphql.data.is_some());
         } else {
             panic!("expected graphql response")
         }

+        // You could keep calling result.next_response() until it yields None if you are expexting more parts.
+        assert!(result.next_response().await.is_none());
         Ok(())
     }
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1206
### The `apollo-router-core` crate has been merged into `apollo-router` ([PR #1189](https://github.com/apollographql/router/pull/1189))

To upgrade, remove any dependency on the `apollo-router-core` crate from your `Cargo.toml` files and change imports like so:

```diff
- use apollo_router_core::prelude::*;
+ use apollo_router::prelude::*;
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1189


### Fix input validation rules ([PR #1211](https://github.com/apollographql/router/pull/1211))
The GraphQL specification provides two sets of coercion / validation rules, depending on whether we're dealing with inputs or outputs.
We have added validation rules for specified input validations which were not previously implemented.
This is a breaking change since slightly invalid input may have validated before but will now be guarded by newly-introduced validation rules.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1211

## 🚀 Features
### Add trace logs for parsing recursion consumption ([PR #1222](https://github.com/apollographql/router/pull/1222))
The `apollo-parser` package now implements recursion limits which can be examined after the parsing phase. The router logs these
out at `trace` level. You can see them in your logs by searching for "`recursion_limit`". For example, when using JSON logging
and using `jq` to filter the output:

```
router -s ../graphql/supergraph.graphql -c ./router.yaml --log trace | jq -c '. | select(.fields.message == "recursion limit data")'
{"timestamp":"2022-06-10T15:01:02.213447Z","level":"TRACE","fields":{"message":"recursion limit data","recursion_limit":"recursion limit: 4096, high: 0"},"target":"apollo_router::spec::schema"}
{"timestamp":"2022-06-10T15:01:02.261092Z","level":"TRACE","fields":{"message":"recursion limit data","recursion_limit":"recursion limit: 4096, high: 0"},"target":"apollo_router::spec::schema"}
{"timestamp":"2022-06-10T15:01:07.642977Z","level":"TRACE","fields":{"message":"recursion limit data","recursion_limit":"recursion limit: 4096, high: 4"},"target":"apollo_router::spec::query"}
```

This example output shows that the maximum recursion limit is 4096 and that the query we processed caused us to recurse 4 times.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1222

### Helm chart now has the option to use an existing secrets for API key [PR #1196](https://github.com/apollographql/router/pull/1196)

This change allows the use of an already existing secret for the graph API key.

To use existing secrets, update your own `values.yaml` file or specify the value on your `helm install` command line.  For example:

```
helm install --set router.managedFederation.existingSecret="my-secret-name" <etc...>`
```

By [@pellizzetti](https://github.com/pellizzetti) in https://github.com/apollographql/router/pull/1196

### Add iterators to `Context` ([PR #1202](https://github.com/apollographql/router/pull/1202))
Context can now be iterated over, with two new methods:

 - `iter()`
 - `iter_mut()`

These implementations lean heavily on an underlying [`DashMap`](https://docs.rs/dashmap/5.3.4/dashmap/struct.DashMap.html#method.iter) implemetation, so refer to its documentation for more usage details.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1202

### Add an experimental optimization to deduplicate variables in query planner ([PR #872](https://github.com/apollographql/router/pull/872))
Get rid of duplicated variables in requests and responses of the query planner. This optimization is disabled by default, if you want to enable it you just need override your configuration:

```yaml title="router.yaml"
plugins:
  experimental.traffic_shaping:
    variables_deduplication: true # Enable the variables deduplication optimization
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/872

### Add more customizable metrics ([PR #1159](https://github.com/apollographql/router/pull/1159))

Added the ability to apply custom attributes/labels to metrics which are derived from header values using the Router's configuration file.  For example:

```yaml
telemetry:
  metrics:
    common:
      attributes:
        static:
          - name: "version"
            value: "v1.0.0"
        from_headers:
          - named: "content-type"
            rename: "payload_type"
            default: "application/json"
          - named: "x-custom-header-to-add"
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1159

### Allow to set a custom health check path ([PR #1164](https://github.com/apollographql/router/pull/1164))
Added the possibility to set a custom health check path
```yaml
server:
  # Default is /.well-known/apollo/server-health
  health_check_path: /health
```

By [@jcaromiq](https://github.com/jcaromiq) in https://github.com/apollographql/router/pull/1164

## 🐛 Fixes

### Pin `clap` dependency in `Cargo.toml` ([PR #1232](https://github.com/apollographql/router/pull/1232))

A minor release of `Clap` occured yesterday which introduced a breaking change.  This change might lead `cargo scaffold` users to hit a panic a runtime when the router tries to parse environment variables and arguments.

This patch pins the `clap` dependency to the version that was available before the release, until the root cause is found and fixed upstream.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1232

### Display better error message when on subgraph fetch errors ([PR #1201](https://github.com/apollographql/router/pull/1201))

Show a helpful error message when a subgraph does not return JSON or a bad status code

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1201

### Fix CORS configuration to eliminate runtime panic on misconfiguration ([PR #1197](https://github.com/apollographql/router/pull/1197))

Previously, it was possible to specify a CORS configuration which was syntactically valid, but which could not be enforced at runtime.  For example, consider the following *invalid* configuration where the `allow_any_origin` and `allow_credentials` parameters are inherantly incompatible with each other (per the CORS specification):

```yaml
server:
  cors:
    allow_any_origin: true
    allow_credentials: true
```

Previously, this would result in a runtime panic. The router will now detect this kind of misconfiguration and report the error without panicking.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1197

## 🛠 Maintenance

### Fix a flappy test to test custom health check path ([PR #1176](https://github.com/apollographql/router/pull/1176))
Force the creation of `SocketAddr` to use a new unused port to avoid port collisions during testing.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1176

### Add static `@skip`/`@include` directive support ([PR #1185](https://github.com/apollographql/router/pull/1185))

- Rewrite the `InlineFragment` implementation
- Add support of static check for `@include` and `@skip` directives

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1185

### Update `buildstructor` to 0.3 ([PR #1207](https://github.com/apollographql/router/pull/1207))

Update `buildstructor` to v0.3.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1207

# [0.9.3] - 2022-06-01

## ❗ BREAKING ❗

## 🚀 Features
### Scaffold custom binary support ([PR #1104](https://github.com/apollographql/router/pull/1104))

Added CLI support for scaffolding a new Router binary project. This provides a starting point for people who want to use the Router as a library and create their own plugins

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1104

### rhai `Context::upsert()` supported with example ([Issue #648](https://github.com/apollographql/router/issues/648))

Rhai plugins can now interact with `Context::upsert()`. We provide an [example in `./examples/rhai-surrogate-cache-key`](https://github.com/apollographql/router/tree/main/examples/rhai-surrogate-cache-key) to illustrate its use.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1136

### Measure APQ cache hits and registers ([Issue #1014](https://github.com/apollographql/router/issues/1014))

The APQ layer will now report cache hits and misses to Apollo Studio if telemetry is configured

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1117

### Add more information to the `subgraph_request` span ([PR #1119](https://github.com/apollographql/router/pull/1119))

Add a new span only for the subgraph request, with all HTTP and net information needed for the OpenTelemetry specs.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1119

## 🐛 Fixes

### Compute default port in span information ([Issue #1160](https://github.com/apollographql/router/pull/1160)) 

Compute default port in span information for `net.peer.port` regarding the scheme of the request URI.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1160

### Response `Content-Type` is, again, `application/json` ([Issue #636](https://github.com/apollographql/router/issues/636)) 

The router was not setting a `content-type` on client responses. This fix ensures that a `content-type` of `application/json` is set when returning a GraphQL response.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1154

### Prevent memory leaks when tasks are cancelled ([PR #767](https://github.com/apollographql/router/pull/767))

Cancelling a request could put the router in an unresponsive state where the deduplication layer or cache would make subgraph requests hang.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/767

## 🛠 Maintenance

### Use subgraphs deployed on Fly.io in CI ([PR #1090](https://github.com/apollographql/router/pull/1090))

The CI needs some Node.js subgraphs for integration tests, which complicates its setup and increases the run time. By deploying, in advance, those subgraphs on Fly.io, we can simplify the CI run.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1090

### Unpin schemars version ([Issue #1074](https://github.com/apollographql/router/issues/1074))

[`schemars`](https://docs.rs/schemars/latest/schemars/) v0.8.9 caused compile errors due to it validating default types.  This change has, however, been rolled back upstream and we can now depend on `schemars` v0.8.10.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1135

### Update Moka to fix occasional panics on AMD hardware ([Issue #1137](https://github.com/apollographql/router/issues/1137))

Moka has a dependency on Quanta which had an issue with AMD hardware. This is now fixed via https://github.com/moka-rs/moka/issues/119

By [@BrynCooke](https://github.com/BrynCooke) in [`6b20dc85`](https://github.com/apollographql/router/commit/6b20dc8520ca03384a4eabac932747fc3a9358d3)

## 📚 Documentation

### rhai `Context::upsert()` supported with example ([Issue #648](https://github.com/apollographql/router/issues/648))

Rhai documentation now illustrates how to use `Context::upsert()` in rhai code.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1136

# [0.9.2] - 2022-05-20

## ❗ BREAKING ❗

### Simplify Context::upsert() [PR #1073](https://github.com/apollographql/router/pull/1073)
Removes the `default` parameter and requires inserted values to implement `Default`.

## 🚀 Features

### DIY docker images script [PR #1106](https://github.com/apollographql/router/pull/1106)
The `build_docker_image.sh` script shows how to build docker images from our GH release tarballs or from a commit hash/tag against the router repo.

## 🐛 Fixes

### Return top `__typename` field when it's not an introspection query [PR #1102](https://github.com/apollographql/router/pull/1102)
When `__typename` is used at the top of the query in combination with other fields it was not returned in the output.

### Fix the installation and releasing script for Windows [PR #1098](https://github.com/apollographql/router/pull/1098)
Do not put .exe for Windows in the name of the tarball when releasing new version

### Aggregate usage reports in streaming and set the timeout to 5 seconds [PR #1066](https://github.com/apollographql/router/pull/1066)
The metrics plugin was allocating chunks of usage reports to aggregate them right after, this was replaced by a streaming loop. The interval for sending the reports to spaceport was reduced from 10s to 5s.

### Fix the environment variable expansion for telemetry endpoints [PR #1092](https://github.com/apollographql/router/pull/1092)
Adds the ability to use environment variable expansion for the configuration of agent/collector endpoint for Jaeger, OTLP, Datadog.

### Fix the introspection query detection [PR #1100](https://github.com/apollographql/router/pull/1100)
Fix the introspection query detection, for example if you only have `__typename` in the query then it's an introspection query, if it's used with other fields (not prefixed by `__`) then it's not an introspection query.

## 🛠 Maintenance

### Add well known query to `PluginTestHarness` [PR #1114](https://github.com/apollographql/router/pull/1114)
Add `call_canned` on `PluginTestHarness`. It performs a well known query that will generate a valid response.

### Remove the batching and timeout from spaceport  [PR #1080](https://github.com/apollographql/router/pull/1080)
Apollo Router is already handling report aggregation and sends the report every 5s. Now spaceport will put the incoming reports in a bounded queue and send them in order, with backpressure.

## 📚 Documentation

### Add CORS documentation ([PR #1044](https://github.com/apollographql/router/pull/1044))
Updated the CORS documentation to reflect the recent [CORS and CSRF](https://github.com/apollographql/router/pull/1006) updates.


# [0.9.1] - 2022-05-17

## ❗ BREAKING ❗

### Remove command line options `--apollo-graph-key` and `--apollo-graph-ref` [PR #1069](https://github.com/apollographql/router/pull/1069)
Using these command lime options exposes sensitive data in the process list. Setting via environment variables is now the only way that these can be set.
In addition these setting have also been removed from the telemetry configuration in yaml.

## 🐛 Fixes
### Pin schemars version to 0.8.8 [PR #1075](https://github.com/apollographql/router/pull/1075)
The Schemars 0.8.9 causes compile errors due to it validating default types. Pin the version to 0.8.8.
See issue [#1074](https://github.com/apollographql/router/issues/1074)

### Fix infinite recursion on during parsing [PR #1078](https://github.com/apollographql/router/pull/1078)
During parsing of queries the use of `"` in a parameter value caused infinite recursion. This preliminary fix will be revisited shortly.
## 📚 Documentation

### Document available metrics in Prometheus [PR #1067](https://github.com/apollographql/router/pull/1067)
Add the list of metrics you can have using Prometheus

# [v0.9.0] - 2022-05-13

## 🎉 **The Apollo Router has graduated from _Preview_ to _General Availability (GA)_!** 🎉

We're so grateful for all the feedback we've received from our early Router adopters and we're excited to bring the Router to our General Availability (GA) release.

We hope you continue to report your experiences and bugs to our team as we continue to move things forward.  If you're having any problems adopting the Router or finding the right migration path from Apollo Gateway which isn't already covered [in our migration guide](https://www.apollographql.com/docs/router/migrating-from-gateway), please open an issue or discussion on this repository!

## ❗ BREAKING ❗

### Remove the agent endpoint configuration for Zipkin [PR #1025](https://github.com/apollographql/router/pull/1025)

Zipkin only supports `endpoint` URL configuration rather than `endpoint` within `collector`, this means Zipkin configuration changes from:

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

A [Cross-Site Request Forgery (CSRF) protection plugin](https://developer.mozilla.org/en-US/docs/Glossary/CSRF) is now enabled by default.

This means [simple requests](https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS#simple_requests) will be rejected from now on, since they represent security risks without the correct CSRF protections in place.

The plugin can be customized as explained in the [CORS and CSRF example](https://github.com/apollographql/router/tree/main/examples/cors-and-csrf/custom-headers.router.yaml).

### CORS default behavior update [PR #1006](https://github.com/apollographql/router/pull/1006)

The CORS `allow_headers` default behavior has changed from its previous configuration.

The Router will now _reflect_ the values received in the [`Access-Control-Request-Headers`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Access-Control-Request-Headers) header, rather than only allowing `Content-Type`, `apollographql-client-name` and `apollographql-client-version` as it did previously.

This change loosens the CORS-related headers restrictions, so it shouldn't have any impact on your setup.

## 🚀 Features

### CSRF Protection [PR #1006](https://github.com/apollographql/router/pull/1006)
The router now embeds a CSRF protection plugin, which is enabled by default. Have a look at the [CORS and CSRF example](https://github.com/apollographql/router/tree/main/examples/cors-and-csrf/custom-headers.router.yaml) to learn how to customize it. [Documentation](https://www.apollographql.com/docs/router/configuration/cors/) will be updated soon!

### Helm chart now supports prometheus metrics [PR #1005](https://github.com/apollographql/router/pull/1005)
The router has supported exporting prometheus metrics for a while. This change updates our helm chart to enable router deployment prometheus metrics. 

Configure by updating your values.yaml or by specifying the value on your helm install command line.

e.g.: helm install --set router.configuration.telemetry.metrics.prometheus.enabled=true <etc...>

> Note: Prometheus metrics are not enabled by default in the helm chart.

### Extend capabilities of rhai processing engine [PR #1021](https://github.com/apollographql/router/pull/1021)

- Rhai plugins can now interact more fully with responses, including **body and header manipulation** where available.
- Closures are now supported for callback processing.
- Subgraph services are now identified by name.

There is more documentation about how to use the various rhai interfaces to the Router and we now have _six_ [examples of rhai scripts](https://github.com/apollographql/router/tree/main/examples) (look for examples prefixed with `rhai-`) doing various request and response manipulations!

## 🐛 Fixes

### Remove the requirement on `jq` in our install script [PR #1034](https://github.com/apollographql/router/pull/1034)

We're now using `cut` command instead of `jq` which allows using our installer without installing `jq` first.  (Don't get us wrong, we love `jq`, but not everyone has it installed!).

### Configuration for Jaeger/Zipkin agent requires an URL instead of a socket address [PR #1018](https://github.com/apollographql/router/pull/1018)
The router now supports URLs for a Jaeger **or** Zipkin agent allowing configuration as follows in this `jaeger` example:

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
Using the `reqwest` blocking client feature was causing panicking due to an incompatible usage of an asynchronous runtime.

### Improvements to Apollo Studio reporting [PR #1020](https://github.com/apollographql/router/pull/1020), [PR #1037](https://github.com/apollographql/router/pull/1037)

This architectural change, which moves the location that we do aggregations internally in the Router, allows us to move towards full reporting functionality.  It shouldn't affect most users.

### Field usage reporting is now reported against the correct schema [PR #1043](https://github.com/apollographql/router/pull/1043)

When using Managed Federation, we now report usage identified by the schema it was processed on, improving reporting in Apollo Studio.

### Check that an object's `__typename` is part of the schema [PR #1033](https://github.com/apollographql/router/pull/1033)

In case a subgraph returns an object with a `__typename` field referring to a type that is not in the API schema, as is the case when using the `@inaccessible` directive on object types, the requested object tree is now replaced with a `null` value in order to conform with the API schema.  This improves our behavior with the recently launched Contracts feature from Apollo Studio.

## 🛠 Maintenance

### OpenTracing examples [PR #1015](https://github.com/apollographql/router/pull/1015)

We now have complete examples of OpenTracing usage with Datadog, Jaeger and Zipkin, that can be started with docker-compose.

## 📚 Documentation
### Add documentation for the endpoint configuration in server ([PR #1000](https://github.com/apollographql/router/pull/1000))

Documentation about setting a custom endpoint path for GraphQL queries has been added.

Also, we reached issue / pull-request number ONE THOUSAND! (💯0)

# [v0.9.0-rc.0] - 2022-05-10

## 🎉 **The Apollo Router has graduated to its Release Candidate (RC) phase!** 🎉

We're so grateful for all the feedback we've received from our early Router adopters and we're excited to bring things even closer to our General Availability (GA) release.

We hope you continue to report your experiences and bugs to our team as we continue to move things forward.  If you're having any problems adopting the Router or finding the right migration path from Apollo Gateway which isn't already covered [in our migration guide](https://www.apollographql.com/docs/router/migrating-from-gateway), please open an issue or discussion on this repository!
## ❗ BREAKING ❗

### Renamed environment variables for consistency [PR #990](https://github.com/apollographql/router/pull/990) [PR #992](https://github.com/apollographql/router/pull/992)

We've adjusted the environment variables that the Router supports to be consistently prefixed with `APOLLO_` and to remove some inconsistencies in their previous naming.

You'll need to adjust to the new environment variable names, as follows:

- `RUST_LOG` -> `APOLLO_ROUTER_LOG`
- `CONFIGURATION_PATH` -> `APOLLO_ROUTER_CONFIG_PATH`
- `SUPERGRAPH_PATH` -> `APOLLO_ROUTER_SUPERGRAPH_PATH`
- `ROUTER_HOT_RELOAD` -> `APOLLO_ROUTER_HOT_RELOAD`
- `APOLLO_SCHEMA_CONFIG_DELIVERY_ENDPOINT` -> `APOLLO_UPLINK_ENDPOINTS`
- `APOLLO_SCHEMA_POLL_INTERVAL`-> `APOLLO_UPLINK_POLL_INTERVAL`

In addition, the following command line flags have changed:
- `--apollo-schema-config-delivery-endpoint` -> `--apollo-uplink-url`
- `--apollo-schema-poll-interval` -> `--apollo-uplink-poll-interval`

### Configurable URL request path [PR #976](https://github.com/apollographql/router/pull/976)

The default router endpoint is now `/` (previously, it was `/graphql`). It's now possible to customize that value by defining an `endpoint` in your Router configuration file's `server` section:

```yaml
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
  # Default is /
  endpoint: /graphql
```

If you necessitated the previous behavior (using `/graphql`), you should use the above configuration.

### Do even more with rhai scripts  [PR #971](https://github.com/apollographql/router/pull/971)

The rhai scripting support in the Router has been re-worked to bring its capabilities closer to that native Rust plugin.  This includes full participation in the service plugin lifecycle and new capabilities like logging support!

See our [`examples`](https://github.com/apollographql/router/tree/main/examples/) directory and [the documentation](https://www.apollographql.com/docs/router/customizations/rhai) for updated examples of how to use the new capabilities.

## 🚀 Features

### Did we already mention doing more with rhai?

It's listed as a breaking change above because it is, but it's worth highlighting that it's now possible to do even more using rhai scripting which previously necessitated writing native Rust plugins and compiling your own binary.

See our [`examples`](https://github.com/apollographql/router/tree/main/examples/) directory and [the documentation](https://www.apollographql.com/docs/router/customizations/rhai) for updated examples of how to use the new capabilities.

### Panics now output to the console [PR #1001](https://github.com/apollographql/router/pull/1001) [PR #1004](https://github.com/apollographql/router/pull/1004)
Previously, panics would get swallowed but are now output to the console/logs.  The use of the Rust-standard environment variables `RUST_BACKTRACE=1` (or `RUST_BACKTRACE=full`) will result in emitting the full backtrace.

### Apollo Studio Usage Reporting [PR #898](https://github.com/apollographql/router/pull/898)
If you have [enabled telemetry in the Router](https://www.apollographql.com/docs/router/configuration/apollo-telemetry#enabling-usage-reporting), you can now see field usage reporting for your queries by heading to the Fields page for your graph in Apollo Studio.

Learn more about our field usage reporting in the Studio [documentation for field usage](https://www.apollographql.com/docs/studio/metrics/field-usage).

### `PluginTestHarness` [PR #898](https://github.com/apollographql/router/pull/898)
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
## 🐛 Fixes

### Improve the diagnostics when encountering a configuration error [PR #963](https://github.com/apollographql/router/pull/963)
In the case of unrecognized properties in your Router's configuration, we will now point you directly to the unrecognized value.  Previously, we pointed to the parent property even if it wasn't the source of the misconfiguration.

### Only allow mutations on HTTP POST requests [PR #975](https://github.com/apollographql/router/pull/975)
Mutations are now only accepted when using the HTTP POST method.

### Fix incorrectly omitting content of interface's fragment [PR #949](https://github.com/apollographql/router/pull/949)
The Router now distinguishes between fragments on concrete types and interfaces.
If an interface is encountered and  `__typename` is being queried, we now check that the returned type implements the interface.

### Set the service name if not specified in config or environment [PR #960](https://github.com/apollographql/router/pull/960)
The router now sets `router` as the default service name in OpenTelemetry traces, along with `process.executable_name`.   This can be adjusted through the configuration file or environment variables.

### Accept an endpoint URL without scheme for telemetry [PR #964](https://github.com/apollographql/router/pull/964)

Endpoint configuration for Datadog and OTLP take a URL as argument, but was incorrectly recognizing addresses of the format "host:port" (i.e., without a scheme, like `grpc://`) as the wrong protocol.  This has been corrected!

### Stricter application of `@inaccessible` [PR #985](https://github.com/apollographql/router/pull/985)

The Router's query planner has been updated to v2.0.2 and stricter behavior for the `@inaccessible` directive.  This also fully supports the new [Apollo Studio Contracts](https://www.apollographql.com/docs/studio/contracts/) feature which just went generally available (GA).

### Impose recursion limits on selection processing [PR #995](https://github.com/apollographql/router/pull/995)

We now limit operations to a depth of 512 to prevent cycles.

## 🛠 Maintenance

### Use official SPDX license identifier for Elastic License v2 (ELv2) [Issue #418](https://github.com/apollographql/router/issues/418)

Rather than pointing to our `LICENSE` file, we now use the `Elastic-2.0` SPDX license identifier to indicate that a particular component is governed by the Elastic License 2.0 (ELv2).  This should facilitate automated compatibility with licensing tools which assist with compliance.

## 📚 Documentation

### Router startup messaging now includes version and license notice  [PR #986](https://github.com/apollographql/router/pull/986)

We now display the version of the Router at startup, along with clarity that the Router is licensed under [ELv2](https://go.apollo.dev/elv2).

# [v0.1.0-preview.7] - 2022-05-04
## ❗ BREAKING ❗

### Plugin utilities cleanup [PR #819](https://github.com/apollographql/router/pull/819), [PR #908](https://github.com/apollographql/router/pull/908)
Utilities around creating Request and Response structures have been migrated to builders.

Migration:
* `plugin_utils::RouterRequest::builder()`->`RouterRequest::fake_builder()`
* `plugin_utils::RouterResponse::builder()`->`RouterResponse::fake_builder()`

In addition, the `plugin_utils` module has been removed. Mock service functionality has been migrated to `plugin::utils::test`.

### Layer cleanup [PR #950](https://github.com/apollographql/router/pull/950)
Reusable layers have all been moved to `apollo_router_core::layers`. In particular the `checkpoint_*` layers have been moved from the `plugins` module.
`async_checkpoint` has been renamed to `checkpoint_async` for consistency with Tower.
Layers that were internal to our execution pipeline have been moved and made private to the crate.

### Plugin API changes [PR #855](https://github.com/apollographql/router/pull/855)
Previously the Plugin trait has three lifecycle hooks: new, startup, and shutdown.

Startup and shutdown are problematic because:
* Plugin construction happens in new and startup. This means creating in new and populating in startup.
* Startup and shutdown has to be explained to the user.
* Startup and shutdown ordering is delicate.

The lifecycle now looks like this:
1. `new`
2. `activate`
3. `drop`

Users can migrate their plugins using the following:
* `Plugin#startup`->`Plugin#new`
* `Plugin#shutdown`->`Drop#drop`

In addition, the `activate` lifecycle hook is now not marked as deprecated, and users are free to use it.

## 🚀 Features

### Add SpanKind and SpanStatusCode to follow the opentelemetry spec [PR #925](https://github.com/apollographql/router/pull/925)
Spans now contains [`otel.kind`](https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/trace/api.md#spankind) and [`otel.status_code`](https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/trace/api.md#set-status) attributes when needed to follow the opentelemtry spec .

###  Configurable client identification headers [PR #850](https://github.com/apollographql/router/pull/850)
The router uses the HTTP headers `apollographql-client-name` and `apollographql-client-version` to identify clients in Studio telemetry. Those headers can now be overriden in the configuration:
```yaml title="router.yaml"
telemetry:
  apollo:
    # Header identifying the client name. defaults to apollographql-client-name
    client_name_header: <custom_client_header_name>
    # Header identifying the client version. defaults to apollographql-client-version
    client_version_header: <custom_version_header_name>
```

## 🐛 Fixes
### Fields in the root selection set of a query are now correctly skipped and included [PR #931](https://github.com/apollographql/router/pull/931)
The `@skip` and `@include` directives are now executed for the fields in the root selection set.

### Configuration errors on hot-reload are output [PR #850](https://github.com/apollographql/router/pull/850)
If a configuration file had errors on reload these were silently swallowed. These are now added to the logs.

### Telemetry spans are no longer created for healthcheck requests [PR #938](https://github.com/apollographql/router/pull/938)
Telemetry spans where previously being created for the healthcheck requests which was creating noisy telemetry for users.

### Dockerfile now allows overriding of `CONFIGURATION_PATH` [PR #948](https://github.com/apollographql/router/pull/948)
Previously `CONFIGURATION_PATH` could not be used to override the config location as it was being passed by command line arg.

## 🛠 Maintenance
### Upgrade `test-span` to display more children spans in our snapshots [PR #942](https://github.com/apollographql/router/pull/942)
Previously in test-span before the fix [introduced here](https://github.com/apollographql/test-span/pull/13) we were filtering too aggressively. So if we wanted to snapshot all `DEBUG` level if we encountered a `TRACE` span which had `DEBUG` children then these children were not snapshotted. It's now fixed and it's more consistent with what we could have/see in jaeger.

### Finalize migration from Warp to Axum [PR #920](https://github.com/apollographql/router/pull/920)
Adding more tests to be more confident to definitely delete the `warp-server` feature and get rid of `warp`

### End to end integration tests for Jaeger [PR #850](https://github.com/apollographql/router/pull/850)
Jaeger tracing end to end test including client->router->subgraphs

### Router tracing span cleanup [PR #850](https://github.com/apollographql/router/pull/850)
Spans generated by the Router are now aligned with plugin services.

### Simplified CI for windows [PR #850](https://github.com/apollographql/router/pull/850)
All windows processes are spawned via xtask rather than a separate CircleCI stage.

### Enable default feature in graphql_client [PR #905](https://github.com/apollographql/router/pull/905)
Removing the default feature can cause build issues in plugins.

### Do not remove __typename from the aggregated response [PR #919](https://github.com/apollographql/router/pull/919)
If the client was explicitely requesting the `__typename` field, it was removed from the aggregated subgraph data, and so was not usable by fragment to check the type.

### Follow the GraphQL spec about Response format [PR #926](https://github.com/apollographql/router/pull/926)
The response's `data` field can be null or absent depending on conventions that are now followed by the router.

## Add client awareness headers to CORS allowed headers [PR #917](https://github.com/apollographql/router/pull/917)

The client awareness headers are now added by default to the list of CORS allowed headers, for easier integration of browser based applications. We also document how to override them and update the CORS configuration accordingly.

## Remove unnecessary box in instrumentation layer [PR #940](https://github.com/apollographql/router/pull/940)

Minor simplification of code to remove boxing during instrumentation.

## 📚 Documentation
### Enhanced rust docs ([PR #819](https://github.com/apollographql/router/pull/819))
Many more rust docs have been added.

### Federation version support page [PR #896](https://github.com/apollographql/router/pull/896)
Add Federation version support doc page detailing which versions of federation are compiled against versions of the router.

### Improve readme for embedded Router [PR #936](https://github.com/apollographql/router/pull/936)
Add more details about pros and cons so that users know what they're letting themselves in for.

### Document layers [PR #950](https://github.com/apollographql/router/pull/950)
Document the notable existing layers and add rust docs for custom layers including basic use cases.

# [v0.1.0-preview.6] - 2022-04-21
## 🐛 Fixes

### Restore the health check route [#883](https://github.com/apollographql/router/issues/883)
Axum rework caused the healthckeck route `/.well-known/apollo/server-health` to change. The route is now restored.

### Correctly flag incoming POST requests [#865](https://github.com/apollographql/router/issues/865)
A regression happened during our recent switch to Axum that would propagate incoming POST requests as GET requests. Fixed and added regression tests.

# [v0.1.0-preview.5] - 2022-04-20
## 🚀 Features
### Helm chart for the router [PR #861](https://github.com/apollographql/router/pull/861)

[Helm](https://helm.sh) support provided by @damienpontifex.

### Line precise error reporting [PR #830](https://github.com/apollographql/router/pull/782)
The router will make a best effort to give line precise error reporting if the configuration was invalid.
```yaml
1. /telemetry/tracing/trace_config/sampler

telemetry:
  tracing:
    trace_config:
      service_name: router3
      sampler: "0.3"
               ^----- "0.3" is not valid under any of the given schemas
```
### Install experience [PR #820](https://github.com/apollographql/router/pull/820)

Added an install script that will automatically download and unzip the router into the local directory.
For more info see the quickstart documentation.

## 🐛 Fixes

### Fix concurrent query planning [#846](https://github.com/apollographql/router/issues/846)
The query planner has been reworked to make sure concurrent plan requests will be dispatched to the relevant requester.

### Do not hang when tracing provider was not set as global [#849](https://github.com/apollographql/router/issues/847)
The telemetry plugin will now Drop cleanly when the Router service stack fails to build.

### Propagate error extensions originating from subgraphs [PR #839](https://github.com/apollographql/router/pull/839)
Extensions are now propagated following the configuration of the `include_subgraph_error` plugin.

### Telemetry configuration [PR #830](https://github.com/apollographql/router/pull/830)
Jaeger and Zipkin telemetry config produced JSON schema that was invalid.

### Return a better error when introspection is disabled [PR #751](https://github.com/apollographql/router/pull/751)
Instead of returning an error coming from the query planner, we are now returning a proper error explaining that the introspection has been disabled.

### Add operation name to subquery fetches [PR #840](https://github.com/apollographql/router/pull/840)
If present in the query plan fetch node, the operation name will be added to sub-fetches.

### Remove trailing slash from Datadog agent endpoint URL [PR #863](https://github.com/apollographql/router/pull/863)
Due to the way the endpoint URL is constructed in opentelemetry-datadog, we cannot set the agent endpoint to a URL with a trailing slash.

## 🛠 Maintenance
### Configuration files validated [PR #830](https://github.com/apollographql/router/pull/830)
Router configuration files within the project are now largely validated via unit test.

### Switch web server framework from `warp` to `axum` [PR #751](https://github.com/apollographql/router/pull/751)
The router is now running by default with an [axum](https://github.com/tokio-rs/axum/) web server instead of `warp`.

### Improve the way we handle Request with axum [PR #845](https://github.com/apollographql/router/pull/845) [PR #877](https://github.com/apollographql/router/pull/877)
Take advantages of new extractors given by `axum`.


# [v0.1.0-preview.4] - 2022-04-11
## ❗ BREAKING ❗
- **Telemetry simplification** [PR #782](https://github.com/apollographql/router/pull/782)

  Telemetry configuration has been reworked to focus exporters rather than OpenTelemetry. Users can focus on what they are trying to integrate with rather than the fact that OpenTelemetry is used in the Apollo Router under the hood.

  ```yaml
  telemetry:
    apollo:
      endpoint:
      apollo_graph_ref:
      apollo_key:
    metrics:
      prometheus:
        enabled: true
    tracing:
      propagation:
        # Propagation is automatically enabled for any exporters that are enabled,
        # but you can enable extras. This is mostly to support otlp and opentracing.
        zipkin: true
        datadog: false
        trace_context: false
        jaeger: false
        baggage: false
  
      otlp:
        endpoint: default
        protocol: grpc
        http:
          ..
        grpc:
          ..
      zipkin:
        agent:
          endpoint: default
      jaeger:
        agent:
          endpoint: default
      datadog:
        endpoint: default
  ```
## 🚀 Features
- **Datadog support** [PR #782](https://github.com/apollographql/router/pull/782)

  Datadog support has been added via `telemetry` yaml configuration.

- **Yaml env variable expansion** [PR #782](https://github.com/apollographql/router/pull/782)

  All values in the router configuration outside the `server` section may use environment variable expansion.
  Unix style expansion is used. Either:

  * `${ENV_VAR_NAME}`- Expands to the environment variable `ENV_VAR_NAME`.
  * `${ENV_VAR_NAME:some_default}` - Expands to `ENV_VAR_NAME` or `some_default` if the environment variable did not exist.

  Only values may be expanded (not keys):
  ```yaml {4,8} title="router.yaml"
  example:
    passord: "${MY_PASSWORD}" 
  ```
## 🐛 Fixes

- **Accept arrays in keys for subgraph joins** [PR #822](https://github.com/apollographql/router/pull/822)

  The router is now accepting arrays as part of the key joining between subgraphs.


- **Fix value shape on empty subgraph queries** [PR #827](https://github.com/apollographql/router/pull/827)

  When selecting data for a federated query, if there is no data the router will not perform the subgraph query and will instead return a default value. This value had the wrong shape and was generating an object where the query would expect an array.

## 🛠 Maintenance

- **Apollo federation 2.0.0 compatible query planning** [PR#828](https://github.com/apollographql/router/pull/828)

  Now that Federation 2.0 is available, we have updated the query planner to use the latest release (@apollo/query-planner v2.0.0).


# [v0.1.0-preview.3] - 2022-04-08
## 🚀 Features
- **Add version flag to router** ([PR #805](https://github.com/apollographql/router/pull/805))

  You can now provider a `--version or -V` flag to the router. It will output version information and terminate.

- **New startup message** ([PR #780](https://github.com/apollographql/router/pull/780))

  The router startup message was updated with more links to documentation and version information.

- **Add better support of introspection queries** ([PR #802](https://github.com/apollographql/router/pull/802))

  Before this feature the Router didn't execute all the introspection queries, only a small number of the most common ones were executed. Now it detects if it's an introspection query, tries to fetch it from cache, if it's not in the cache we execute it and put the response in the cache.

- **Add an option to disable the landing page** ([PR #801](https://github.com/apollographql/router/pull/801))

  By default the router will display a landing page, which could be useful in development. If this is not
  desirable the router can be configured to not display this landing page:
  ```yaml
  server:
    landing_page: false
  ```

- **Add support of metrics in `apollo.telemetry` plugin** ([PR #738](https://github.com/apollographql/router/pull/738))

  The Router will now compute different metrics you can expose via Prometheus or OTLP exporter.

  Example of configuration to export an endpoint (configured with the path `/plugins/apollo.telemetry/metrics`) with metrics in `Prometheus` format:

  ```yaml
  telemetry:
    metrics:
      exporter:
        prometheus:
          # By setting this endpoint you enable the prometheus exporter
          # All our endpoints exposed by plugins are namespaced by the name of the plugin
          # Then to access to this prometheus endpoint, the full url path will be `/plugins/apollo.telemetry/metrics`
          endpoint: "/metrics"
    ```

- **Add experimental support of `custom_endpoint` method in `Plugin` trait** ([PR #738](https://github.com/apollographql/router/pull/738))

  The `custom_endpoint` method lets you declare a new endpoint exposed for your plugin. For now it's only accessible for official `apollo.` plugins and for `experimental.`. The return type of this method is a Tower [`Service`]().
  
- **configurable subgraph error redaction** ([PR #797](https://github.com/apollographql/router/issues/797))
  By default, subgraph errors are not propagated to the user. This experimental plugin allows messages to be propagated either for all subgraphs or on
  an individual subgraph basis. Individual subgraph configuration overrides the default (all) configuration. The configuration mechanism is similar
  to that used in the `headers` plugin:
  ```yaml
  plugins:
    experimental.include_subgraph_errors:
      all: true
  ```

- **Add a trace level log for subgraph queries** ([PR #808](https://github.com/apollographql/router/issues/808))

  To debug the query plan execution, we added log messages to print the query plan, and for each subgraph query,
  the operation, variables and response. It can be activated as follows:

  ```
  router -s supergraph.graphql --log info,apollo_router_core::query_planner::log=trace
  ```

## 🐛 Fixes
- **Eliminate memory leaks when tasks are cancelled** [PR #758](https://github.com/apollographql/router/pull/758)

  The deduplication layer could leak memory when queries were cancelled and never retried: leaks were previously cleaned up on the next similar query. Now the leaking data will be deleted right when the query is cancelled

- **Trim the query to better detect an empty query** ([PR #738](https://github.com/apollographql/router/pull/738))

  Before this fix, if you wrote a query with only whitespaces inside, it wasn't detected as an empty query.

- **Keep the original context in `RouterResponse` when returning an error** ([PR #738](https://github.com/apollographql/router/pull/738))

  This fix keeps the original http request in `RouterResponse` when there is an error.

- **add a user-agent header to the studio usage ingress submission** ([PR #773](https://github.com/apollographql/router/pull/773))

  Requests to Studio now identify the router and its version

## 🛠 Maintenance
- **A faster Query planner** ([PR #768](https://github.com/apollographql/router/pull/768))

  We reworked the way query plans are generated before being cached, which lead to a great performance improvement. Moreover, the router is able to make sure the schema is valid at startup and on schema update, before you query it.

- **Xtask improvements** ([PR #604](https://github.com/apollographql/router/pull/604))

  The command we run locally to make sure tests, lints and compliance-checks pass will now edit the license file and run cargo fmt so you can directly commit it before you open a Pull Request

- **Switch from reqwest to a Tower client for subgraph services** ([PR #769](https://github.com/apollographql/router/pull/769))

  It results in better performance due to less URL parsing, and now header propagation falls under the apollo_router_core log filter, making it harder to disable accidentally

- **Remove OpenSSL usage** ([PR #783](https://github.com/apollographql/router/pull/783) and [PR #810](https://github.com/apollographql/router/pull/810))

  OpenSSL is used for HTTPS clients when connecting to subgraphs or the Studio API. It is now replaced with rustls, which is faster to compile and link

- **Download the Studio protobuf schema during build** ([PR #776](https://github.com/apollographql/router/pull/776)

  The schema was vendored before, now it is downloaded dynamically during the build process

- **Fix broken benchmarks** ([PR #797](https://github.com/apollographql/router/issues/797))

  the `apollo-router-benchmarks` project was failing due to changes in the query planner. It is now fixed, and its subgraph mocking code is now available in `apollo-router-core`

## 📚 Documentation

- **Document the Plugin and DynPlugin trait** ([PR #800](https://github.com/apollographql/router/pull/800)

  Those traits are used to extend the router with Rust plugins
  
# [v0.1.0-preview.2] - 2022-04-01
## ❗ BREAKING ❗

- **CORS default Configuration** ([#40](https://github.com/apollographql/router/issues/40))

  The Router will allow only the https://studio.apollographql.com origin by default, instead of any origin.
  This behavior can still be tweaked in the [YAML configuration](https://www.apollographql.com/docs/router/configuration/cors)

- **Hot reload flag** ([766](https://github.com/apollographql/router/issues/766))
  The `--watch` (or `-w`) flag that enables hot reload was renamed to `--hr` or `--hot-reload`

## 🚀 Features

- **Hot reload via en environment variable** ([766](https://github.com/apollographql/router/issues/766))
  You can now use the `ROUTER_HOT_RELOAD=true` environment variable to have the router watch for configuration and schema changes and automatically reload.

- **Container images are now available** ([PR #764](https://github.com/apollographql/router/pull/764))

  We now build container images More details at:
    https://github.com/apollographql/router/pkgs/container/router

  You can use the images with docker, for example, as follows:
    e.g.: docker pull ghcr.io/apollographql/router:v0.1.0-preview.1

  The images are based on [distroless](https://github.com/GoogleContainerTools/distroless) which is a very constrained image, intended to be secure and small.

  We'll provide release and debug images for each release. The debug image has a busybox shell which can be accessed using (for instance) `--entrypoint=sh`.

  For more details about these images, see the docs.

- **Skip and Include directives in post processing** ([PR #626](https://github.com/apollographql/router/pull/626))

  The Router now understands the [@skip](https://spec.graphql.org/October2021/#sec--skip) and [@include](https://spec.graphql.org/October2021/#sec--include) directives in queries, to add or remove fields depending on variables. It works in post processing, by filtering fields after aggregating the subgraph responses.

- **Add an option to deactivate introspection** ([PR #749](https://github.com/apollographql/router/pull/749))

  While schema introspection is useful in development, we might not want to expose the entire schema in production,
  so the router can be configured to forbid introspection queries as follows:
  ```yaml
  server:
    introspection: false
  ```

## 🐛 Fixes
- **Move query dedup to an experimental `traffic_shaping` plugin** ([PR #753](https://github.com/apollographql/router/pull/753))

  The experimental `traffic_shaping` plugin will be a central location where we can add things such as rate limiting and retry.

- **Remove `hasNext` from our response objects** ([PR #733](https://github.com/apollographql/router/pull/733))

  `hasNext` is a field in the response that may be used in future to support features such as defer and stream. However, we are some way off supporting this and including it now may break clients. It has been removed.

- **Extend Apollo uplink configurability** ([PR #741](https://github.com/apollographql/router/pull/741))

  Uplink url and poll interval can now be configured via command line arg and env variable:
  ```bash
    --apollo-schema-config-delivery-endpoint <apollo-schema-config-delivery-endpoint>
      The endpoint polled to fetch the latest supergraph schema [env: APOLLO_SCHEMA_CONFIG_DELIVERY_ENDPOINT=]

    --apollo-schema-poll-interval <apollo-schema-poll-interval>
      The time between polls to Apollo uplink. Minimum 10s [env: APOLLO_SCHEMA_POLL_INTERVAL=]  [default: 10s]
  ```
  In addition, other existing uplink env variables are now also configurable via arg. 

- **Make deduplication and caching more robust against cancellation** [PR #752](https://github.com/apollographql/router/pull/752) [PR #758](https://github.com/apollographql/router/pull/758)

  Cancelling a request could put the router in an unresponsive state where the deduplication layer or cache would make subgraph requests hang.

- **Relax variables selection for subgraph queries** ([PR #755](https://github.com/apollographql/router/pull/755))

  Federated subgraph queries relying on partial or invalid data from previous subgraph queries could result in response failures or empty subgraph queries. The router is now more flexible when selecting data from previous queries, while still keeping a correct form for the final response

## 🛠 Maintenance

## 📚 Documentation

# [v0.1.0-preview.1] - 2022-03-23

## 🎉 **The Apollo Router has graduated to its Preview phase!** 🎉
## ❗ BREAKING ❗

- **Improvements to telemetry attribute YAML ergonomics** ([PR #729](https://github.com/apollographql/router/pull/729))

  Trace config YAML ergonomics have been improved. To add additional attributes to your trace information, you can now use the following format:

  ```yaml
        trace_config:
          attributes:
            str: "a"
            int: 1
            float: 1.0
            bool: true
            str_arr:
              - "a"
              - "b"
            int_arr:
              - 1
              - 2
            float_arr:
              - 1.0
              - 2.0
            bool_arr:
              - true
              - false
  ```
## 🐛 Fixes

- **Log and error message formatting** ([PR #721](https://github.com/apollographql/router/pull/721))

  Logs and error messages now begin with lower case and do not have trailing punctuation, per Rust conventions.

- **OTLP default service.name and service.namespace** ([PR #722](https://github.com/apollographql/router/pull/722))

  While the Jaeger YAML configuration would default to `router` for the `service.name` and to `apollo` for the `service.namespace`, it was not the case when using a configuration that utilized OTLP. This lead to an `UNKNOWN_SERVICE` name span in zipkin traces, and difficult to find Jaeger traces.

# [v0.1.0-preview.0] - 2022-03-22

## 🎉 **The Apollo Router has graduated to its Preview phase!** 🎉

For more information on what's expected at this stage, please see our [release stages](https://www.apollographql.com/docs/resources/release-stages/#preview).

## 🐛 Fixes

- **Header propagation by `name` only fixed** ([PR #709](https://github.com/apollographql/router/pull/709))

  Previously `rename` and `default` values were required (even though they were correctly not flagged as required in the json schema).
  The following will now work:
  ```yaml
  headers:
    all:
    - propagate:
        named: test
  ```
- **Fix OTLP hang on reload** ([PR #711](https://github.com/apollographql/router/pull/711))

  Fixes hang when OTLP exporter is configured and configuration hot reloads.

# [v0.1.0-alpha.10] 2022-03-21

## ❗ BREAKING ❗

- **Header propagation `remove`'s `name` is now `named`** ([PR #674](https://github.com/apollographql/router/pull/674))

  This merely renames the `remove` options' `name` setting to be instead `named` to be a bit more intuitively named and consistent with its partner configuration, `propagate`.

  _Previous configuration_

  ```yaml
    # Remove a named header
    - remove:
      name: "Remove" # Was: "name"
  ```
  _New configuration_

  ```yaml
    # Remove a named header
    - remove:
      named: "Remove" # Now: "named"
  ```

- **Command-line flag vs Environment variable precedence changed** ([PR #693](https://github.com/apollographql/router/pull/693))

  For logging related verbosity overrides, the `RUST_LOG` environment variable no longer takes precedence over the command line argument.  The full order of precedence is now command-line argument overrides environment variable overrides the default setting.

## 🚀 Features

- **Forbid mutations plugin** ([PR #641](https://github.com/apollographql/router/pull/641))

  The forbid mutations plugin allows you to configure the router so that it disallows mutations.  Assuming none of your `query` requests are mutating data or changing state (they shouldn't!) this plugin can be used to effectively make your graph read-only. This can come in handy when testing the router, for example, if you are mirroring/shadowing traffic when trying to validate a Gateway to Router migration! 😸

- **⚠️ Add experimental Rhai plugin** ([PR #484](https://github.com/apollographql/router/pull/484))

  Add an _experimental_ core plugin to be able to extend Apollo Router functionality using [Rhai script](https://rhai.rs/). This allows users to write their own `*_service` function similar to how as you would with a native Rust plugin but without needing to compile a custom router. Rhai scripts have access to the request context and headers directly and can make simple manipulations on them.

  See our [Rhai script documentation](https://www.apollographql.com/docs/router/customizations/rhai) for examples and details!

## 🐛 Fixes

- **Correctly set the URL path of the HTTP request in `RouterRequest`** ([Issue #699](https://github.com/apollographql/router/issues/699))

  Previously, we were not setting the right HTTP path on the `RouterRequest` so when writing a plugin with `router_service` you always had an empty path `/` on `RouterRequest`.

## 📚 Documentation

- **We have incorporated a substantial amount of documentation** (via many, many PRs!)

  See our improved documentation [on our website](https://www.apollographql.com/docs/router/).

# [v0.1.0-alpha.9] 2022-03-16
## ❗ BREAKING ❗

- **Header propagation configuration changes** ([PR #599](https://github.com/apollographql/router/pull/599))

  Header manipulation configuration is now a core-plugin and configured at the _top-level_ of the Router's configuration file, rather than its previous location within service-level layers.  Some keys have also been renamed.  For example:

  **Previous configuration**

  ```yaml
  subgraphs:
    products:
      layers:
        - headers_propagate:
            matching:
              regex: .*
  ```

  **New configuration**

  ```yaml
  headers:
    subgraphs:
      products:
        - propagate:
          matching: ".*"
  ```

- **Move Apollo plugins to top-level configuration** ([PR #623](https://github.com/apollographql/router/pull/623))

  Previously plugins were all under the `plugins:` section of the YAML config.  However, these "core" plugins are now promoted to the top-level of the config. This reflects the fact that these plugins provide core functionality even though they are implemented as plugins under the hood and further reflects the fact that they receive special treatment in terms of initialization order (they are initialized first before members of `plugins`).

- **Remove configurable layers** ([PR #603](https://github.com/apollographql/router/pull/603))

  Having `plugins` _and_ `layers` as configurable items in YAML was creating confusion as to when it was appropriate to use a `layer` vs a `plugin`.  As the layer API is a subset of the plugin API, `plugins` has been kept, however the `layer` option has been dropped.

- **Plugin names have dropped the `com.apollographql` prefix** ([PR #602](https://github.com/apollographql/router/pull/600))

  Previously, core plugins were prefixed with `com.apollographql.`.  This is no longer the case and, when coupled with the above moving of the core plugins to the top-level, the prefixing is no longer present.  This means that, for example, `com.apollographql.telemetry` would now be just `telemetry`.

- **Use `ControlFlow` in checkpoints** ([PR #602](https://github.com/apollographql/router/pull/602))


- **Add Rhai plugin** ([PR #548](https://github.com/apollographql/router/pull/484))

  Both `checkpoint` and `async_checkpoint` now `use std::ops::ControlFlow` instead of the `Step` enum.  `ControlFlow` has two variants, `Continue` and `Break`.

- **The `reporting` configuration changes to `telemetry`** ([PR #651](https://github.com/apollographql/router/pull/651))

  All configuration that was previously under the `reporting` header is now under a `telemetry` key.
## :sparkles: Features

- **Header propagation now supports "all" subgraphs** ([PR #599](https://github.com/apollographql/router/pull/599))

  It is now possible to configure header propagation rules for *all* subgraphs without needing to explicitly name each subgraph.  You can accomplish this by using the `all` key, under the (now relocated; see above _breaking changes_) `headers` section.

  ```yaml
  headers:
    all:
    - propagate:
      matching: "aaa.*"
    - propagate:
      named: "bbb"
      default: "def"
      rename: "ccc"
    - insert:
      name: "ddd"
      value: "eee"
    - remove:
      matching: "fff.*"
    - remove:
      name: "ggg"
  ```

- **Update to latest query planner from Federation 2** ([PR #653](https://github.com/apollographql/router/pull/653))

  The Router now uses the `@apollo/query-planner@2.0.0-preview.5` query planner, bringing the most recent version of Federation 2.

## 🐛 Fixes

- **`Content-Type` of HTTP responses is now set to `application/json`** ([Issue #639](https://github.com/apollographql/router/issues/639))

  Previously, we were not setting a `content-type` on HTTP responses.  While plugins can still set a different `content-type` if they'd like, we now ensure that a `content-type` of `application/json` is set when one was not already provided.

- **GraphQL Enums in query parameters** ([Issue #612](https://github.com/apollographql/router/issues/612))

  Enums in query parameters were handled correctly in the response formatting, but not in query validation.  We now have a new test and a fix.

- **OTel trace propagation works again** ([PR #620](https://github.com/apollographql/router/pull/620))

  When we re-worked our OTel implementation to be a plugin, the ability to trace across processes (into subgraphs) was lost. This fix restores this capability.  We are working to improve our end-to-end testing of this to prevent further regressions.

- **Reporting plugin schema generation** ([PR #607](https://github.com/apollographql/router/pull/607))

  Previously our `reporting` plugin configuration was not able to participate in JSON Schema generation. This is now broadly correct and makes writing a syntactically-correct schema much easier.

  To generate a schema, you can still run the same command as before:

  ```
  router --schema > apollo_configuration_schema.json
  ```

  Then, follow the instructions for associating it with your development environment.

- **Input object validation** ([PR #658](https://github.com/apollographql/router/pull/658))

  Variable validation was incorrectly using output objects instead of input objects

# [v0.1.0-alpha.8] 2022-03-08

## :sparkles: Features

- **Request lifecycle checkpoints** ([PR #558](https://github.com/apollographql/router/pull/548) and [PR #580](https://github.com/apollographql/router/pull/548))

    Checkpoints in the request pipeline now allow plugin authors (which includes us!) to check conditions during a request's lifecycle and circumvent further execution if desired.
    
    Using `Step` return types within the checkpoint it's possible to influence what happens (including changing things like the HTTP status code, etc.).  A caching layer, for example, could return `Step::Return(response)` if a cache "hit" occurred and `Step::Continue(request)` (to allow normal processing to continue) in the event of a cache "miss".
    
    These can be either synchronous or asynchronous.  To see examples, see:
    
    - A [synchronous example](https://github.com/apollographql/router/tree/190afe181bf2c50be1761b522fcbdcc82b81d6ca/examples/forbid-anonymous-operations)
    - An [asynchronous example](https://github.com/apollographql/router/tree/190afe181bf2c50be1761b522fcbdcc82b81d6ca/examples/async-allow-client-id)

- **Contracts support** ([PR #573](https://github.com/apollographql/router/pull/573))

  The Apollo Router now supports [Apollo Studio Contracts](https://www.apollographql.com/docs/studio/contracts/)!

- **Add OpenTracing support** ([PR #548](https://github.com/apollographql/router/pull/548))

  OpenTracing support has been added into the reporting plugin.  You're now able to have span propagation (via headers) via two common formats supported by the `opentracing` crate: `zipkin_b3` and `jaeger`.


## :bug: Fixes

- **Configuration no longer requires `router_url`** ([PR #553](https://github.com/apollographql/router/pull/553))

  When using Managed Federation or directly providing a Supergraph file, it is no longer necessary to provide a `routing_url` value.  Instead, the values provided by the Supergraph or Studio will be used and the `routing_url` can be used only to override specific URLs for specific subgraphs.

- **Fix plugin ordering** ([PR #559](https://github.com/apollographql/router/issues/559))

  Plugins need to execute in sequence of declaration *except* for certain "core" plugins (e.g., reporting) which must execute early in the plugin sequence to make sure they are in place as soon as possible in the Router lifecycle. This change now ensures that the reporting plugin executes first and that all other plugins are executed in the order of declaration in configuration.

- **Propagate Router operation lifecycle errors** ([PR #537](https://github.com/apollographql/router/issues/537))

  Our recent extension rework was missing a key part: Error propagation and handling! This change makes sure errors that occurred during query planning and query execution will be displayed as GraphQL errors instead of an empty payload.


# [v0.1.0-alpha.7] 2022-02-25

## :sparkles: Features

- **Apollo Studio Explorer landing page** ([PR #526](https://github.com/apollographql/router/pull/526))

  We've replaced the _redirect_ to Apollo Studio with a statically rendered landing page.  This supersedes the previous redirect approach was merely introduced as a short-cut.  The experience now duplicates the user-experience which exists in Apollo Gateway today.

  It is also possible to _save_ the redirect preference and make the behavior sticky for future visits.  As a bonus, this also resolves the failure to preserve the correct HTTP scheme (e.g., `https://`) in the event that the Apollo Router was operating behind a TLS-terminating proxy, since the redirect is now handled client-side.

  Overall, this should be a more durable and more transparent experience for the user.

- **Display Apollo Router version on startup** ([PR #543](https://github.com/apollographql/router/pull/543))
  The Apollo Router displays its version on startup from now on, which will come in handy when debugging/observing how your application behaves.

## :bug: Fixes

- **Passing a `--supergraph` file supersedes Managed Federation** ([PR #535](https://github.com/apollographql/router/pull/535))

  The `--supergraph` flag will no longer be silently ignored when the Supergraph is already being provided through [Managed Federation](https://www.apollographql.com/docs/federation/managed-federation/overview) (i.e., when the `APOLLO_KEY` and `APOLLO_GRAPH_REF` environment variables are set).  This allows temporarily overriding the Supergraph schema that is fetched from Apollo Studio's Uplink endpoint, while still reporting metrics to Apollo Studio reporting ingress.

- **Anonymous operation names are now empty in tracing** ([PR #525](https://github.com/apollographql/router/pull/525))

  When GraphQL operation names are not necessary to execute an operation (i.e., when there is only a single operation in a GraphQL document) and the GraphQL operation is _not_ named (i.e., it is anonymous), the `operation_name` attribute on the trace spans that are associated with the request will no longer contain a single hyphen character (`-`) but will instead be an empty string.  This matches the way that these operations are represented during the GraphQL operation's life-cycle as well.

- **Resolved missing documentation in Apollo Explorer** ([PR #540](https://github.com/apollographql/router/pull/540))

   We've resolved a scenario that prevented Apollo Explorer from displaying documentation by adding support for a new introspection query which also queries for deprecation (i.e., `includeDeprecated`) on `input` arguments.
  
# [v0.1.0-alpha.6] 2022-02-18

## :sparkles: Features

- **Apollo Studio Managed Federation support** ([PR #498](https://github.com/apollographql/router/pull/498))

  [Managed Federation]: https://www.apollographql.com/docs/federation/managed-federation/overview/

  The Router can now automatically download and check for updates on its schema from Studio (via [Uplink])'s free, [Managed Federation] service.  This is configured in the same way as Apollo Gateway via the `APOLLO_KEY` and `APOLLO_GRAPH_REF` environment variables, in the same way as was true in Apollo Gateway ([seen here](https://www.apollographql.com/docs/federation/managed-federation/setup/#4-connect-the-gateway-to-studio)). This will also enable operation usage reporting.

  > **Note:** It is not yet possible to configure the Router with [`APOLLO_SCHEMA_CONFIG_DELIVERY_ENDPOINT`].  If you need this behavior, please open a feature request with your use case.

  [`APOLLO_SCHEMA_CONFIG_DELIVERY_ENDPOINT`]: https://www.apollographql.com/docs/federation/managed-federation/uplink/#environment-variable
  [Uplink]: https://www.apollographql.com/docs/federation/managed-federation/uplink/
  [operation usage reporting]: https://www.apollographql.com/docs/studio/metrics/usage-reporting/#pushing-metrics-from-apollo-server

- **Subgraph header configuration** ([PR #453](https://github.com/apollographql/router/pull/453))

  The Router now supports passing both client-originated and router-originated headers to specific subgraphs using YAML configuration.  Each subgraph which needs to receive headers can specify which headers (or header patterns) should be forwarded to which subgraph.

  More information can be found in our documentation on [subgraph header configuration].

  At the moment, when using using YAML configuration alone, router-originated headers can only be static strings (e.g., `sent-from-apollo-router: true`).  If you have use cases for deriving headers in the router dynamically, please open or find a feature request issue on the repository which explains the use case.

  [subgraph header configuration]: https://www.apollographql.com/docs/router/configuration/#configuring-headers-received-by-subgraphs

- **In-flight subgraph `query` de-duplication** ([PR #285](https://github.com/apollographql/router/pull/285))

  As a performance booster to both the Router and the subgraphs it communicates with, the Router will now _de-duplicate_ multiple _identical_ requests to subgraphs when there are multiple in-flight requests to the same subgraph with the same `query` (**never** `mutation`s), headers, and GraphQL `variables`.  Instead, a single request will be made to the subgraph and the many client requests will be served via that single response.

  There may be a substantial drop in number of requests observed by subgraphs with this release.

- **Operations can now be made via `GET` requests** ([PR #429](https://github.com/apollographql/router/pull/429))

  The Router now supports `GET` requests for `query` operations.  Previously, the Apollo Router only supported making requests via `POST` requests.  We've always intended on supporting `GET` support, but needed some additional support in place to make sure we could prevent allowing `mutation`s to happen over `GET` requests.

- **Automatic persisted queries (APQ) support** ([PR #433](https://github.com/apollographql/router/pull/433))

  The Router now handles [automatic persisted queries (APQ)] by default, as was previously the case in Apollo Gateway.  APQ support pairs really well with `GET` requests (which also landed in this release) since they allow read operations (e.g., `GET` requests) to be more easily cached by intermediary proxies and CDNs, which typically forbid caching `POST` requests by specification (even if they often are just reads in GraphQL).  Follow the link above to the documentation to test them out.

  [automatic persisted queries (APQ)]: https://www.apollographql.com/docs/apollo-server/performance/apq/

- **New internal Tower architecture and preparation for extensibility** ([PR #319](https://github.com/apollographql/router/pull/319))

  We've introduced new foundational primitives to the Router's request pipeline which facilitate the creation of composable _onion layers_.  For now, this is largely leveraged through a series of internal refactors and we'll need to document and expand on more of the details that facilitate developers building their own custom extensions.  To leverage existing art &mdash; and hopefully maximize compatibility and facilitate familiarity &mdash; we've leveraged the [Tokio Tower `Service`] pattern.

  This should facilitate a number of interesting extension opportunities and we're excited for what's in-store next.  We intend on improving and iterating on the API's ergonomics for common Graph Router behaviors over time, and we'd encourage you to open issues on the repository with use-cases you might think need consideration.

  [Tokio Tower `Service`]: https://docs.rs/tower/latest/tower/trait.Service.html

- **Support for Jaeger HTTP collector in OpenTelemetry** ([PR #479](https://github.com/apollographql/router/pull/479))

  It is now possible to configure Jaeger HTTP collector endpoints within the `opentelemetry` configuration.  Previously, Router only supported the UDP method.

  The [documentation] has also been updated to demonstrate how this can be configured.

  [documentation]: https://www.apollographql.com/docs/router/configuration/#using-jaeger

## :bug: Fixes

- **Studio agent collector now binds to localhost** [PR #486](https://github.com/apollographql/router/pulls/486)

  The Studio agent collector will bind to `127.0.0.1`.  It can be configured to bind to `0.0.0.0` if desired (e.g., if you're using the collector to collect centrally) by using the [`spaceport.listener` property] in the documentation.

  [`spaceport.listener` property]: https://www.apollographql.com/docs/router/configuration/#spaceport-configuration

# [v0.1.0-alpha.5] 2022-02-15

## :sparkles: Features

- **Apollo Studio usage reporting agent and operation-level reporting** ([PR #309](https://github.com/apollographql/router/pulls/309), [PR #420](https://github.com/apollographql/router/pulls/420))

  While there are several levels of Apollo Studio integration, the initial phase of our Apollo Studio reporting focuses on operation-level reporting.

  At a high-level, this will allow Apollo Studio to have visibility into some basic schema details, like graph ID and variant, and per-operation details, including:
  
  - Overall operation latency
  - The number of times the operation is executed
  - [Client awareness] reporting, which leverages the `apollographql-client-*` headers to give visibility into _which clients are making which operations_.

  This should enable several Apollo Studio features including the _Clients_ and _Checks_ pages as well as the _Checks_ tab on the _Operations_ page.
  
  > *Note:* As a current limitation, the _Fields_ page will not have detailed field-based metrics and on the _Operations_ page the _Errors_ tab, the _Traces_ tab and the _Error Percentage_ graph will not receive data.  We recommend configuring the Router's [OpenTelemetry tracing] with your APM provider and using distributed tracing to increase visibility into individual resolver performance.

  Overall, this marks a notable but still incremental progress toward more of the Studio integrations which are laid out in [#66](https://github.com/apollographql/router/issues/66).

  [Client awareness]: https://www.apollographql.com/docs/studio/metrics/client-awareness/
  [Schema checks]: https://www.apollographql.com/docs/studio/schema-checks/
  [OpenTelemetry tracing]: https://www.apollographql.com/docs/router/configuration/#tracing

- **Complete GraphQL validation** ([PR #471](https://github.com/apollographql/router/pull/471) via [federation-rs#37](https://github.com/apollographql/federation-rs/pull/37))

  We now apply all of the standard validations which are defined in the `graphql` (JavaScript) implementation's default set of "[specified rules]" during query planning.

  [specified rules]: https://github.com/graphql/graphql-js/blob/95dac43fd4bff037e06adaa7cfb44f497bca94a7/src/validation/specifiedRules.ts#L76-L103

## :bug: Fixes

- **No more double `http://http://` in logs** ([PR #448](https://github.com/apollographql/router/pulls/448))

  The server logs will no longer advertise the listening host and port with a doubled-up `http://` prefix.  You can once again click happily into Studio Explorer!

- **Improved handling of Federation 1 supergraphs** ([PR #446](https://github.com/apollographql/router/pull/446) via [federation#1511](https://github.com/apollographql/federation/pull/1511))

  Our partner team has improved the handling of Federation 1 supergraphs in the implementation of Federation 2 alpha (which the Router depends on and is meant to offer compatibility with Federation 1 in most cases).  We've updated our query planner implementation to the version with the fixes.

  This also was the first time that we've leveraged the new [`federation-rs`] repository to handle our bridge, bringing a huge developmental advantage to teams working across the various concerns!

  [`federation-rs`]: https://github.com/apollographql/federation-rs

- **Resolved incorrect subgraph ordering during merge** ([PR #460](https://github.com/apollographql/router/pull/460))

  A fix was applied to fix the behavior which was identified in [Issue #451] which was caused by a misconfigured filter which was being applied to field paths.

  [Issue #451]: https://github.com/apollographql/router/issues/451
# [v0.1.0-alpha.4] 2022-02-03

## :sparkles: Features

- **Unix socket support** via [#158](https://github.com/apollographql/router/issues/158)

  _...and via upstream [`tokios-rs/tokio#4385`](https://github.com/tokio-rs/tokio/pull/4385)_

  The Router can now listen on Unix domain sockets (i.e., IPC) in addition to the existing IP-based (port) listening.  This should bring further compatibility with upstream intermediaries who also allow support this form of communication!

  _(Thank you to [@cecton](https://github.com/cecton), both for the PR that landed this feature but also for contributing the upstream PR to `tokio`.)_

## :bug: Fixes

- **Resolved hangs occurring on Router reload when `jaeger` was configured** via [#337](https://github.com/apollographql/router/pull/337)

  Synchronous calls being made to [`opentelemetry::global::set_tracer_provider`] were causing the runtime to misbehave when the configuration (file) was adjusted (and thus, hot-reloaded) on account of the root context of that call being asynchronous.

  This change adjusts the call to be made from a new thread.  Since this only affected _potential_ runtime configuration changes (again, hot-reloads on a configuration change), the thread spawn is  a reasonable solution.

  [`opentelemetry::global::set_tracer_provider`]: https://docs.rs/opentelemetry/0.10.0/opentelemetry/global/fn.set_tracer_provider.html

## :nail_care: Improvements

> Most of the improvements this time are internal to the code-base but that doesn't mean we shouldn't talk about them.  A great developer experience matters both internally and externally! :smile_cat:

- **Store JSON strings in a `bytes::Bytes` instance** via [#284](https://github.com/apollographql/router/pull/284)

  The router does a a fair bit of deserialization, filtering, aggregation and re-serializing of JSON objects.  Since we currently operate on a dynamic schema, we've been relying on [`serde_json::Value`] to represent this data internally.

  After this change, that `Value` type is now replaced with an equivalent type from a new [`serde_json_bytes`], which acts as an envelope around an underlying `bytes::Bytes`.  This allows us to refer to the buffer that contained the JSON data while avoiding the allocation and copying costs on each string for values that are largely unused by the Router directly.

  This should offer future benefits when implementing &mdash; e.g., query de-duplication and caching &mdash; since a single buffer will be usable by multiple responses at the same time.

  [`serde_json::Value`]: https://docs.rs/serde_json/0.9.8/serde_json/enum.Value.html
  [`serde_json_bytes`]: https://crates.io/crates/serde_json_bytes
  [`bytes::Bytes`]: https://docs.rs/bytes/0.4.12/bytes/struct.Bytes.html

-  **Development workflow improvement** via [#367](https://github.com/apollographql/router/pull/367)

   Polished away some existing _Problems_ reported by `rust-analyzer` and added troubleshooting instructions to our documentation.

- **Removed unnecessary `Arc` from `PreparedQuery`'s `execute`** via [#328](https://github.com/apollographql/router/pull/328)

  _...and followed up with [#367](https://github.com/apollographql/router/pull/367)_

- **Bumped/upstream improvements to `test_span`** via [#359](https://github.com/apollographql/router/pull/359)

  _...and [`apollographql/test-span#11`](https://github.com/apollographql/test-span/pull/11) upstream_

  Internally, this is just a version bump to the Router, but it required upstream changes to the `test-span` crate.  The bump brings new filtering abilities and adjusts the verbosity of spans tracing levels, and removes non-determinism from tests.

# [v0.1.0-alpha.3] 2022-01-11

## :rocket::waxing_crescent_moon: Public alpha release

> An alpha or beta release is in volatile, active development. The release might not be feature-complete, and breaking API changes are possible between individual versions.

## :sparkles: Features

- Trace sampling [#228](https://github.com/apollographql/router/issues/228): Tracing each request can be expensive. The router now supports sampling, which allows us to only send a fraction of the received requests.

- Health check [#54](https://github.com/apollographql/router/issues/54)

## :bug: Fixes

- Schema parse errors [#136](https://github.com/apollographql/router/pull/136): The router wouldn't display what went wrong when parsing an invalid Schema. It now displays exactly where a the parsing error occured, and why.

- Various tracing and telemetry fixes [#237](https://github.com/apollographql/router/pull/237): The router wouldn't display what went wrong when parsing an invalid Schema. It now displays exactly where a the parsing error occured, and why.

- Query variables validation [#62](https://github.com/apollographql/router/issues/62): Now that we have a schema parsing feature, we can validate the variables and their types against the schemas and queries.


# [v0.1.0-alpha.2] 2021-12-03

## :rocket::waxing_crescent_moon: Public alpha release

> An alpha or beta release is in volatile, active development. The release might not be feature-complete, and breaking API changes are possible between individual versions.

## :sparkles: Features

- Add support for JSON Logging [#46](https://github.com/apollographql/router/issues/46)

## :bug: Fixes

- Fix Open Telemetry report errors when using Zipkin [#180](https://github.com/apollographql/router/issues/180)

# [v0.1.0-alpha.1] 2021-11-18

## :rocket::waxing_crescent_moon: Initial public alpha release

> An alpha or beta release is in volatile, active development. The release might not be feature-complete, and breaking API changes are possible between individual versions.

See our [release stages] for more information.

## :sparkles: Features

This release focuses on documentation and bug fixes, stay tuned for the next releases!

## :bug: Fixes

- Handle commas in the @join\_\_graph directive parameters [#101](https://github.com/apollographql/router/pull/101)

There are several accepted syntaxes to define @join\_\_graph parameters. While we did handle whitespace separated parameters such as `@join__graph(name: "accounts" url: "http://accounts/graphql")`for example, we discarded the url in`@join__graph(name: "accounts", url: "http://accounts/graphql")` (notice the comma). This pr fixes that.

- Invert subgraph URL override logic [#135](https://github.com/apollographql/router/pull/135)

Subservices endpoint URLs can both be defined in `supergraph.graphql` and in the subgraphs section of the `configuration.yml` file. The configuration now correctly overrides the supergraph endpoint definition when applicable.

- Parse OTLP endpoint address [#156](https://github.com/apollographql/router/pull/156)

The router OpenTelemetry configuration only supported full URLs (that contain a scheme) while OpenTelemtry collectors support full URLs and endpoints, defaulting to `https`. This pull request fixes that.

## :books: Documentation

A lot of configuration examples and links have been fixed ([#117](https://github.com/apollographql/router/pull/117), [#120](https://github.com/apollographql/router/pull/120), [#133](https://github.com/apollographql/router/pull/133))

## :pray: Thank you!

Special thanks to @sjungling, @hsblhsn, @martin-dd, @Mithras and @vvakame for being pioneers by trying out the router, opening issues and documentation fixes! :rocket:

# [v0.1.0-alpha.0] 2021-11-10

## :rocket::waxing_crescent_moon: Initial public alpha release

> An alpha or beta release is in volatile, active development. The release might not be feature-complete, and breaking API changes are possible between individual versions.

See our [release stages] for more information.

[release stages]: https://www.apollographql.com/docs/resources/release-stages/

## :sparkles: Features

- **Federation 2 alpha**

  The Apollo Router supports the new alpha features of [Apollo Federation 2], including its improved shared ownership model and enhanced type merging.  As new Federation 2 features are released, we will update the Router to bring in that new functionality.
  
  [Apollo Federation 2]: https://www.apollographql.com/blog/announcement/backend/announcing-federation-2/

- **Supergraph support**

  The Apollo Router supports supergraphs that are published to the Apollo Registry, or those that are composed locally.  Both options are enabled by using [Rover] to produce (`rover supergraph compose`) or fetch (`rover supergraph fetch`) the supergraph to a file.  This file is passed to the Apollo Router using the `--supergraph` flag.
  
  See the Rover documentation on [supergraphs] for more information!
  
  [Rover]: https://www.apollographql.com/rover/
  [supergraphs]: https://www.apollographql.com/docs/rover/supergraphs/

- **Query planning and execution**

  The Apollo Router supports Federation 2 query planning using the same implementation we use in Apollo Gateway for maximum compatibility.  In the future, we would like to migrate the query planner to Rust.  Query plans are cached in the Apollo Router for improved performance.
  
- **Performance**

  We've created benchmarks demonstrating the performance advantages of a Rust-based Apollo Router. Early results show a substantial performance improvement over our Node.js based Apollo Gateway, with the possibility of improving performance further for future releases. 
  
  Additionally, we are making benchmarking an integrated part of our CI/CD pipeline to allow us to monitor the changes over time.  We hope to bring awareness of this into the public purview as we have new learnings.
  
  See our [blog post] for more.
  
  [blog post]: https://www.apollographql.com/blog/announcement/backend/apollo-router-our-graphql-federation-runtime-in-rust/
  
- **Apollo Sandbox Explorer**

  [Apollo Sandbox Explorer] is a powerful web-based IDE for creating, running, and managing GraphQL operations.  Visiting your Apollo Router endpoint will take you into the Apollo Sandbox Explorer, preconfigured to operate against your graph. 
  
  [Apollo Sandbox Explorer]: https://www.apollographql.com/docs/studio/explorer/

- **Introspection support**

  Introspection support makes it possible to immediately explore the graph that's running on your Apollo Router using the Apollo Sandbox Explorer.  Introspection is currently enabled by default on the Apollo Router.  In the future, we'll support toggling this behavior.
  
- **OpenTelemetry tracing**

  For enabling observability with existing infrastructure and monitoring performance, we've added support using [OpenTelemetry] tracing. A number of configuration options can be seen in the [configuration][configuration 1] documentation under the `opentelemetry` property which allows enabling Jaeger or [OTLP]. 
  
  In the event that you'd like to send data to other tracing platforms, the [OpenTelemetry Collector] can be run an agent and can funnel tracing (and eventually, metrics) to a number of destinations which are implemented as [exporters].
  
  [configuration 1]: https://www.apollographql.com/docs/router/configuration/#configuration-file
  [OpenTelemetry]: https://opentelemetry.io/
  [OTLP]: https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/protocol/otlp.md
  [OpenTelemetry Collector]: https://github.com/open-telemetry/opentelemetry-collector
  [exporters]: https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/exporter
  
- **CORS customizations**

  For a seamless getting started story, the Apollo Router has CORS support enabled by default with `Access-Control-Allow-Origin` set to `*`, allowing access to it from any browser environment.
  
  This configuration can be adjusted using the [CORS configuration] in the documentation.
  
  [CORS configuration]: https://www.apollographql.com/docs/router/configuration/#handling-cors

- **Subgraph routing URL overrides**

  Routing URLs are encoded in the supergraph, so specifying them explicitly isn't always necessary.
  
  In the event that you have dynamic subgraph URLs, or just want to quickly test something out locally, you can override subgraph URLs in the configuration.
  
  Changes to the configuration will be hot-reloaded by the running Apollo Router.
  
## 📚 Documentation

  The beginnings of the [Apollo Router's documentation] is now available in the Apollo documentation. We look forward to continually improving it!

- **Quickstart tutorial**

  The [quickstart tutorial] offers a quick way to try out the Apollo Router using a pre-deployed set of subgraphs we have running in the cloud.  No need to spin up local subgraphs!  You can of course run the Apollo Router with your own subgraphs too by providing a supergraph.
  
- **Configuration options**

  On our [configuration][configuration 2] page we have a set of descriptions for some common configuration options (e.g., supergraph and CORS) as well as a [full configuration] file example of the currently supported options.  
  
  [quickstart tutorial]: https://www.apollographql.com/docs/router/quickstart/
  [configuration 2]: https://www.apollographql.com/docs/router/configuration/
  [full configuration]: https://www.apollographql.com/docs/router/configuration/#configuration-file

# [v0.1.0-prealpha.5] 2021-11-09

## :rocket: Features

- **An updated `CHANGELOG.md`!**

  As we build out the base functionality for the router, we haven't spent much time updating the `CHANGELOG`. We should probably get better at that!

  This release is the last one before reveal! 🎉

## :bug: Fixes

- **Potentially, many!**

  But the lack of clarity goes back to not having kept track of everything thus far! We can _fix_ our processes to keep track of these things! :smile_cat:

# [0.1.0] - TBA
