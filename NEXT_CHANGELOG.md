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

# [0.16] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Rename map_future_with_context to map_future_with_request_data ([PR #1547](https://github.com/apollographql/router/pull/1547))

The function is not very well named since it's in fact used to extract any data from a request for use in a future. This rename makes it clear.

By [@garypen](https://github.com/garypen)

### Rename traffic shaping deduplication options ([PR #1540](https://github.com/apollographql/router/pull/1540))

In the traffic shaping module:
 - `variables_deduplication` configuration option is renamed to `deduplicate_variables`.
 - `query_deduplication` configuration option is renamed to `deduplicate_query`.

```diff
- traffic_shaping:
-   variables_deduplication: true # Enable the variables deduplication optimization
-   all:
-     query_deduplication: true # Enable query deduplication for all subgraphs.
-   subgraphs:
-     products:
-       query_deduplication: false # Disable query deduplication for products.
+ traffic_shaping:
+   deduplicate_variables: true # Enable the variables deduplication optimization
+   all:
+     deduplicate_query: true # Enable query deduplication for all subgraphs.
+   subgraphs:
+     products:
+       deduplicate_query: false # Disable query deduplication for products.
```

By [@garypen](https://github.com/garypen)

### Put `query_plan_options` in private and wrap `QueryPlanContent` in an opaque type ([PR #1486](https://github.com/apollographql/router/pull/1486))

`QueryPlanOptions::query_plan_options` is no longer available in public.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1486

### Removed `delay_interval` in telemetry configuration. ([PR #1498](https://github.com/apollographql/router/pull/1498))

It was doing nothing.

```yaml title="router.yaml"
telemetry:
  metrics:
    common:
      # Removed, will now cause an error on Router startup:
      delay_interval:
        secs: 9
        nanos: 500000000
```

By [@SimonSapin](https://github.com/SimonSapin)

### Remove telemetry configuration hot reloading ([PR #1463](https://github.com/apollographql/router/pull/1463))

Configuration hot reloading is not very useful for telemetry, and is the
source of regular bugs that are hard to fix.

This removes the support for configuration reloading entirely. Now, the
router will reject a configuration reload with an error log if the
telemetry configuration changed.

It is now possible to create a subscriber and pass it explicitely to the telemetry plugin
when creating it. It will then be modified to integrate the telemetry plugin's layer.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/1463

### Reorder query planner execution ([PR #1484](https://github.com/apollographql/router/pull/1484))

Query planning is deterministic, it only depends on the query, operation name and query planning
options. As such, we can cache the result of the entire process.

This changes the pipeline to apply query planner plugins between the cache and the bridge planner,
so those plugins will only be called once on the same query. If changes must be done per query,
they should happen in a supergraph service.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1464

### Remove Buffer from Mock*Service ([PR #1440](https://github.com/apollographql/router/pull/1440)

This removes the usage of `tower_test::mock::Mock` in mocked services because it isolated the service in a task
so panics triggered by mockall were not transmitted up to the unit test that should catch it.
This rewrites the mocked services API to remove the `build()` method, and make them clonable if needed,
using an `expect_clone` call with mockall.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1440

### Some items were renamed or moved ([PR #1487](https://github.com/apollographql/router/pull/1487))

At the crate root:

* `SchemaKind` → `SchemaSource`
* `SchemaKind::String(String)` → `SchemaSource::Static { schema_sdl: String }`
* `ConfigurationKind` → `ConfigurationSource`
* `ConfigurationKind::Instance` → `ConfigurationSource::Static`
* `ShutdownKind` → `ShutdownSource`
* `ApolloRouter` → `RouterHttpServer`

In the `apollo_router::plugin::Plugin` trait:

* `router_service` → `supergraph_service`
* `query_planning_service` → `query_planner_service`

A new `apollo_router::stages` module replaces `apollo_router::services` in the public API,
reexporting its items and adding `BoxService` and `BoxCloneService` type aliases.
Additionally, the "router stage" is now known as "supergraph stage".
In pseudo-syntax, `use`’ing the old names:

```rust
mod supergraph {
    use apollo_router::services::RouterRequest as Request;
    use apollo_router::services::RouterResponse as Response;
    type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
}

mod query_planner {
    use apollo_router::services::QueryPlannerRequest as Request;
    use apollo_router::services::QueryPlannerResponse as Response;
    type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;

    // Reachable from Request or Response:
    use apollo_router::query_planner::QueryPlan;
    use apollo_router::query_planner::QueryPlanOptions;
    use apollo_router::services::QueryPlannerContent;
    use apollo_router::spec::Query;
}

mod execution {
    use apollo_router::services::ExecutionRequest as Request;
    use apollo_router::services::ExecutionResponse as Response;
    type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
}

mod subgraph {
    use super::*;
    use apollo_router::services::SubgraphRequest as Request;
    use apollo_router::services::SubgraphResponse as Response;
    type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;

    // Reachable from Request or Response:
    use apollo_router::query_planner::OperationKind;
}
```

Migration example:

```diff
-use tower::util::BoxService;
-use tower::BoxError;
-use apollo_router::services::{RouterRequest, RouterResponse};
+use apollo_router::stages::router;
 
-async fn example(service: BoxService<RouterRequest, RouterResponse, BoxError>) -> RouterResponse {
+async fn example(service: router::BoxService) -> router::Response {
-    let request = RouterRequest::builder()/*…*/.build();
+    let request = router::Request::builder()/*…*/.build();
     service.oneshot(request).await
 }
```

By [@SimonSapin](https://github.com/SimonSapin)

### Some items were removed from the public API ([PR #1487](https://github.com/apollographql/router/pull/1487))

If you used some of them and don’t find a replacement,
please [file an issue](https://github.com/apollographql/router/issues/)
with details about the use case.

```
apollo_router::Configuration::boxed
apollo_router::Configuration::is_compatible
apollo_router::errors::CacheResolverError
apollo_router::errors::JsonExtError
apollo_router::errors::ParsesError::print
apollo_router::errors::PlanError
apollo_router::errors::PlannerError
apollo_router::errors::PlannerErrors
apollo_router::errors::QueryPlannerError
apollo_router::errors::ServiceBuildError
apollo_router::json_ext
apollo_router::layers::ServiceBuilderExt::cache
apollo_router::mock_service!
apollo_router::plugins
apollo_router::plugin::plugins
apollo_router::plugin::PluginFactory
apollo_router::plugin::DynPlugin
apollo_router::plugin::Handler
apollo_router::plugin::test::IntoSchema
apollo_router::plugin::test::MockSubgraphFactory
apollo_router::plugin::test::PluginTestHarness
apollo_router::query_planner::QueryPlan::execute
apollo_router::services
apollo_router::Schema
```

By [@SimonSapin](https://github.com/SimonSapin)

### Router startup API changes ([PR #1487](https://github.com/apollographql/router/pull/1487))

The `RouterHttpServer::serve` method and its return type `RouterHandle` were removed,
their functionality merged into `RouterHttpServer` (formerly `ApolloRouter`).
The builder for `RouterHttpServer` now ends with a `start` method instead of `build`.
This method immediatly starts the server in a new Tokio task.

```diff
 RouterHttpServer::builder()
     .configuration(configuration)
     .schema(schema)
-    .build()
-    .serve()
+    .start()
     .await
```

By [@SimonSapin](https://github.com/SimonSapin)

### `router_builder_fn` replaced by `shutdown` in the `Executable` builder ([PR #1487](https://github.com/apollographql/router/pull/1487))

The builder for `apollo_router::Executable` had a `router_builder_fn` method
allowing to specify how a `RouterHttpServer` (previously `ApolloRouter`) was to be created
with a provided configuration and schema.
The only possible variation there was specifying when the server should shut down
with a `ShutdownSource` parameter,
so `router_builder_fn` was replaced with a new `shutdown` method that takes that.

```diff
 use apollo_router::Executable;
-use apollo_router::RouterHttpServer;
 use apollo_router::ShutdownSource;

 Executable::builder()
-    .router_builder_fn(|configuration, schema| RouterHttpServer::builder()
-        .configuration(configuration)
-        .schema(schema)
-        .shutdown(ShutdownSource::None)
-        .start())
+    .shutdown(ShutdownSource::None)
     .start()
     .await
```

By [@SimonSapin](https://github.com/SimonSapin)

### Removed constructors when there is a public builder ([PR #1487](https://github.com/apollographql/router/pull/1487))

Many types in the Router API can be constructed with the builder pattern.
We use the [`buildstructor`](https://crates.io/crates/buildstructor) crate
to auto-generate builder boilerplate based on the parameters of a constructor.
These constructors have been made private so that users must go through the builder instead,
which will allow us to add parameters in the future without a breaking API change.
If you were using one of these constructors, the migration generally looks like this:

```diff
-apollo_router::graphql::Error::new(m, vec![l], Some(p), Default::default())
+apollo_router::graphql::Error::build()
+    .message(m)
+    .location(l)
+    .path(p)
+    .build()
```

By [@SimonSapin](https://github.com/SimonSapin)

### Removed deprecated type aliases ([PR #1487](https://github.com/apollographql/router/pull/1487))

A few versions ago, some types were moved from the crate root to a new `graphql` module.
To help the transition, type aliases were left at the old location with a deprecation warning.
These aliases are now removed, remaining imports must be changed to the new location:

```diff
-use apollo_router::Error;
-use apollo_router::Request;
-use apollo_router::Response;
+use apollo_router::graphql::Error;
+use apollo_router::graphql::Request;
+use apollo_router::graphql::Response;
```

Alternatively, import the module with `use apollo_router::graphql` 
then use qualified paths such as `graphql::Request`.
This can help disambiguate when multiple types share a name.

By [@SimonSapin](https://github.com/SimonSapin)

### `RouterRequest::fake_builder` defaults to `Content-Type: application/json` ([PR #1487](https://github.com/apollographql/router/pull/1487))

`apollo_router::services::RouterRequest` has a builder for creating a “fake” request during tests.
When no `Content-Type` header is specified, this builder will now default to `application/json`.
This will help tests where a request goes through mandatory plugins including CSRF protection.
which makes the request be accepted by CSRF protection.

If a test requires a request specifically *without* a `Content-Type` header,
this default can be removed from a `RouterRequest` after building it:

```rust
let mut router_request = RouterRequest::fake_builder().build();
router_request.originating_request.headers_mut().remove("content-type");
```

By [@SimonSapin](https://github.com/SimonSapin)

### Plugins return a service for custom endpoints ([Issue #1481](https://github.com/apollographql/router/issues/1481))

Rust plugins can implement the `Plugin::custom_endpoint` trait method
to handle some non-GraphQL HTTP requests.

Previously, the return type of this method was `Option<apollo_router::plugin::Handler>`,
where a `Handler` could be created with:

```rust
impl Handler {
    pub fn new(service: tower::util::BoxService<
        apollo_router::http_ext::Request<bytes::Bytes>, 
        apollo_router::http_ext::Response<bytes::Bytes>, 
        tower::BoxError
    >) -> Self {/* … */}
}
```

`Handler` has been removed from the public API, now plugins return a `BoxService` directly instead.
Additionally, the type for HTTP request and response bodies was changed
from `bytes::Bytes` to `hyper::Body` (which is more flexible, for example can be streamed).

Changes needed if using custom enpoints are:

* Replace `Handler::new(service)` with `service`
* To read the full request body,
  use [`hyper::body::to_bytes`](https://docs.rs/hyper/latest/hyper/body/fn.to_bytes.html)
  or [`hyper::body::aggregate`](https://docs.rs/hyper/latest/hyper/body/fn.aggregate.html).
* A response `Body` can be created through conversion traits from various types.
  For example: `"string".into()`

By [@SimonSapin](https://github.com/SimonSapin)

## 🚀 Features

### rhai logging functions now accept Dynamic parameters ([PR #1521](https://github.com/apollographql/router/pull/1521))

Prior to this change, rhai logging functions worked with string parameters. This change means that any valid rhai object
may now be passed as a logging parameter.

By [@garypen](https://github.com/garypen)

### Reduce initial memory footprint by lazily populating introspection query cache ([#1516](https://github.com/apollographql/router/issues/1516))

In an early alpha release of the Router, we only executed certain "known" introspection queries because of prior technical constraints that prohibited us from doing something more flexible.  Because the set of introspection queries was "known", it made sense to cache them.

As of https://github.com/apollographql/router/pull/802, this special-casing is (thankfully) no longer necessary and we no longer need to _know_ (and constrain!) the introspection queries that the Router supports.

We could have kept caching those "known" queries, however we were finding that the resulting cache size was quite large and making the Router's minimum memory footprint larger than need be since we were caching many introspection results which the Router instance would never encounter.

This change removes the cache entirely and allows introspection queries served by the Router to merely be lazily calculated and cached on-demand, thereby reducing the initial memory footprint.  Disabling introspection entirely will prevent any use of this cache since no introspection will be possible.

By [@o0Ignition0o](https://github.com/o0Ignition0o)

### Expose query plan in extensions for GraphQL response (experimental) ([PR #1470](https://github.com/apollographql/router/pull/1470))

Expose query plan in extensions for GraphQL response. Only experimental for now, no documentation available.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1470

### Add support of global rate limit and timeout. [PR #1347](https://github.com/apollographql/router/pull/1347)

Additions to the traffic shaping plugin:
- **Global rate limit** - If you want to rate limit requests to subgraphs or to the router itself.
- **Timeout**: - Set a timeout to subgraphs and router requests.

```yaml
traffic_shaping:
  router: # Rules applied to requests from clients to the router
    global_rate_limit: # Accept a maximum of 10 requests per 5 secs. Excess requests must be rejected.
      capacity: 10
      interval: 5s # Must not be greater than 18_446_744_073_709_551_615 milliseconds and not less than 0 milliseconds
    timeout: 50s # If a request to the router takes more than 50secs then cancel the request (30 sec by default)
  subgraphs: # Rules applied to requests from the router to individual subgraphs
    products:
      global_rate_limit: # Accept a maximum of 10 requests per 5 secs from the router. Excess requests must be rejected.
        capacity: 10
        interval: 5s # Must not be greater than 18_446_744_073_709_551_615 milliseconds and not less than 0 milliseconds
      timeout: 50s # If a request to the subgraph 'products' takes more than 50secs then cancel the request (30 sec by default)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1347

### Explicit `shutdown` for `RouterHttpServer` handle ([PR #1487](https://github.com/apollographql/router/pull/1487))

If you explicitly create a `RouterHttpServer` handle,
dropping it while the server is running instructs the server shut down gracefuly.
However with the handle dropped, there is no way to wait for shutdown to end
or check that it went without error.
Instead, the new `shutdown` async method can be called explicitly
to obtain a `Result`:

```diff
 use RouterHttpServer;
 let server = RouterHttpServer::builder().schema("schema").start();
 // …
-drop(server);
+server.shutdown().await.unwrap(); 
```

By [@SimonSapin](https://github.com/SimonSapin)

### Added `apollo_router::TestHarness` ([PR #1487](https://github.com/apollographql/router/pull/1487))

This is a builder for the part of an Apollo Router that handles GraphQL requests,
as a `tower::Service`.
This allows tests, benchmarks, etc
to manipulate request and response objects in memory without going over the network.
See the API documentation for an example. (It can be built with `cargo doc --open`.)

By [@SimonSapin](https://github.com/SimonSapin)

### Remove telemetry configuration hot reloading ([PR #1501](https://github.com/apollographql/router/pull/1501))

The `map_deferred_response` method is now available for the router service and execution
service in Rhai. When using the `@defer` directive, we get the data in a serie of graphql
responses. The first one is available with the `map_response` method, where the HTTP headers
and the response body can be modified. The following responses are available through
`map_deferred_response`, which only has access to the response body.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/1501

## 🐛 Fixes

### Expose query plan: move the behavior to the execution_service ([#1541](https://github.com/apollographql/router/issues/1541))

There isn't much use for QueryPlanner plugins. Most of the logic done there can be done in `execution_service`. Moreover users could get inconsistent plugin behavior because it depends on whether the QueryPlanner cache hits or not.

By [@o0Ignition0o](https://github.com/o0Ignition0o)

### Accept SIGTERM as shutdown signal ([PR #1497](https://github.com/apollographql/router/pull/1497))

This will make containers stop faster as they will not have to wait until a SIGKILL to stop the router.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1497

### Set the response path for deferred responses ([PR #1529](https://github.com/apollographql/router/pull/1529))

Some GraphQL clients rely on the response path to find out which
fragment created a deferred response, and generate code that checks the
type of the value at that path.
Previously the router was generating a value that starts at the root
for every deferred response. Now it checks the path returned by the query
plan execution and creates a response for each value that matches that
path.
In particular, for deferred fragments on an ojbect inside an array, it
will create a separate response for each element of the array.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1529

## 🛠 Maintenance

### Display licenses.html diff in CI if the check failed ([#1524](https://github.com/apollographql/router/issues/1524))

The CI check that ensures that the `license.html` file is up to date now displays what has changed when the file is out of sync.

By [@o0Ignition0o](https://github.com/o0Ignition0o)

## 🚀 Features

### Helm: Rhai script and Istio virtualservice support ([#1478](https://github.com/apollographql/router/issues/1478))

You can now pass a Rhai script file to the helm chart.
You can also provide an Istio VirtualService configuration, as well as custom Egress rules.
Head over to the helm chart [default values](https://github.com/apollographql/router/blob/main/helm/chart/router/values.yaml) to get started.

By [@o0Ignition0o](https://github.com/o0Ignition0o)

## 📚 Documentation
