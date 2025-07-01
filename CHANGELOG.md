# Changelog

This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).

# [2.4.0] - 2025-06-30

## 🚀 Features

### Support JWT audience (`aud`) validation ([PR #7578](https://github.com/apollographql/router/pull/7578))

The router now supports JWT audience (`aud`) validation. This allows the router to ensure that the JWT is intended
for the specific audience it is being used with, enhancing security by preventing token misuse across different audiences.

The following sample configuration will validate the JWT's `aud` claim against the specified audiences and ensure a match with either `https://my.api` or `https://my.other.api`. If the `aud` claim does not match either of those configured audiences, the router will reject the request.

```yaml
authentication:
 router:
   jwt:
     jwks: # This key is required.
       - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
         issuers: # optional list of issuers
           - https://issuer.one
           - https://issuer.two
         audiences: # optional list of audiences
           - https://my.api
           - https://my.other.api
         poll_interval: <optional poll interval>
         headers: # optional list of static headers added to the HTTP request to the JWKS URL
           - name: User-Agent
             value: router
     # These keys are optional. Default values are shown.
     header_name: Authorization
     header_value_prefix: Bearer
     on_error: Error
     # array of alternative token sources
     sources:
       - type: header
         name: X-Authorization
         value_prefix: Bearer
       - type: cookie
         name: authz
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7578

### Prioritize existing requests over query parsing and planning during "warm up" ([PR #7223](https://github.com/apollographql/router/pull/7223))

The router warms up its query planning cache during a hot reload. This change decreases the priority
of warm up tasks in the compute job queue to reduce the impact of warmup on serving requests.

This change adds new values to the `job.type` dimension of the following metrics:
- `apollo.router.compute_jobs.duration` - A histogram of time spent in the compute pipeline by the job, including the queue and query planning.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)
  - `job.outcome`: (`executed_ok`, `executed_error`, `channel_error`, `rejected_queue_full`, `abandoned`)
- `apollo.router.compute_jobs.queue.wait.duration` - A histogram of time spent in the compute queue by the job.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)
- `apollo.router.compute_jobs.execution.duration` - A histogram of time spent to execute job (excludes time spent in the queue).
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)
- `apollo.router.compute_jobs.active_jobs` - A gauge of the number of compute jobs being processed in parallel.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7223

### Persisted queries: include operation name in `PERSISTED_QUERY_NOT_IN_LIST` error for debuggability ([PR #7768](https://github.com/apollographql/router/pull/7768))

When persisted query safelisting is enabled and a request has an unknown PQ ID, the GraphQL error now has the extension field `operation_name` containing the GraphQL operation name (if provided explicitly in the request). Note that this only applies to the `PERSISTED_QUERY_NOT_IN_LIST` error returned when manifest-based PQs are enabled, APQs are disabled, and the request contains an operation ID that is not in the list.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/7768

## Introduce cooperative cancellation for query planning

The cooperative cancellation feature allows the router to gracefully handle query planning timeouts and cancellations, improving resource utilization.

The `mode` can be set to `measure` or `enforce`. We recommend starting with `measure`. In `measure` mode, the router will measure the time taken for query planning and emit metrics accordingly. In `enforce` mode, the router will cancel query planning operations that exceed the specified timeout.

To observe this behavior, the router telemetry has been updated:

- Add an `outcome` attribute to the `apollo.router.query_planning.plan.duration` metric
- Add an `outcome` attribute to the `query_planning` span

Below is a sample configuration to configure cooperative cancellation in measure mode:

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      enabled: true
      mode: measure
      timeout: 1s
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7604

## 🐛 Fixes

### Align `on_graphql_error` selector with `subgraph_on_graphql_error` ([PR #7676](https://github.com/apollographql/router/pull/7676))

The `on_graphql_error` selector will now return `true` or `false`, in alignment with the `subgraph_on_graphql_error` selector. Previously, the selector would return `true` or `None`.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7676

### Return valid GraphQL response when performing a websocket handshake ([PR #7680](https://github.com/apollographql/router/pull/7680))

[PR #7141](https://github.com/apollographql/router/pull/7141) added checks on GraphQL responses returned from coprocessors to ensure compliance with GraphQL specifications. This surfaced an issue where subscription responses over websockets could omit the required `data` field during the handshake, resulting in invalid GraphQL response payloads. All websocket subscription responses will now return a valid GraphQL response when doing the websocket handshake.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7680

### Fix SigV4 configuration handling ([PR #7726](https://github.com/apollographql/router/pull/7726))

Fixed an issue introduced in Router 2.3.0 where some SigV4 configurations would fail to start, preventing communication with SigV4-enabled services.

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7726

### Improve error message for invalid variables  ([Issue #2984](https://github.com/apollographql/router/issues/2984))

When a variable in a GraphQL request is missing or contains an invalid value, the router now returns more useful error messages. Example:

```diff
-invalid type for variable: 'x'
+invalid input value at x.coordinates[0].longitude: found JSON null for GraphQL Float!
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/7567

### Support exporting resources on all Prometheus metrics ([PR #7394](https://github.com/apollographql/router/pull/7394))

By default, the Prometheus metrics exporter will only export resources as `target_info` metrics, not inline on every metric. Now, you can add resources to every metric by setting `resource_selector` to `all` (default is `none`).

```yaml
telemetry:
  exporters:
    metrics:
      common:
        resource:
          "test-resource": "test"
      prometheus:
        enabled: true
        resource_selector: all # This will add resources on every metrics
```

Note: this change only affects Prometheus, not OTLP.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7394

### Forbid unknown `@link` directives for supergraph schemas where `purpose` is `EXECUTION` or `SECURITY`

The legacy JavaScript query planner forbid any usage of unknown `@link` specs in supergraph schemas with either `EXECUTION` or `SECURITY` value set for the `for` argument (aka, the spec's "purpose"). This behavior had not been ported to the native query planner previously. This PR implements the expected behavior in the native query planner.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/7587

### Supergraph stage correctly receives `on_graphql_error` selector ([PR #7669](https://github.com/apollographql/router/pull/7669))

The `on_graphql_error` selector will now correctly fire on the supergraph stage; previously it only worked on the router stage.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7669

### Invalid type condition in `@defer` fetch

The query planner was adding an inline spread (`...`) conditioned on the `Query` type in deferred subgraph fetch queries. Such a query would be invalid in the subgraph when the subgraph schema renamed the root `query` type to somethhing other than `Query`. The fix removes the root type condition from all subgraph queries, so that they stay valid even when root types are renamed.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/7580

### Preserve `content-type` for file uploads when Rhai scripts are in use ([PR #7559](https://github.com/apollographql/router/pull/7559))

If a Rhai script was invoked during file upload processing, then the "Content-Type" of the request was not preserved correctly. This would cause a file upload to fail.

The error message would be something like:

```
"message": "invalid multipart request: Content-Type is not multipart/form-data",
```

This issue has now been fixed.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7559

### Improve OTLP metric HTTP endpoint behavior ([PR #7595](https://github.com/apollographql/router/pull/7595))

We made substantial updates to OpenTelemetry in router 2.0, but didn't catch that OpenTelemetry changed how it processed "endpoints" (destinations for metrics and traces) until now.

With the undetected change, the router wasn't setting the path correctly, resulting in failure to export metrics over HTTP when using the "default" endpoint. **Neither metrics via gRPC nor traces were impacted**.

We have fixed our interactions with the dependency and improved our testing to make sure this does not occur again.  Additionally, the router now supports setting standard OpenTelemetry environment variables for endpoints.

There is still a known problem when using environment variables to configure endpoints for the HTTP protocol when transmitting to an un-encrypted endpoint (i.e., TLS not configured).  This affects the following environment variables:

- `OTEL_EXPORTER_OTLP_ENDPOINT`
- `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`

When these environment variables are set to insecure hosts, messages will appear in the logs indicating an error, but **the metrics and traces will still be sent correctly**:

```
2025-06-06T15:12:47.992144Z ERROR  OpenTelemetry metric error occurred: Metrics exporter otlp failed with the grpc server returns error (Unknown error): , detailed error message: h2 protocol error: http2 error tonic::transport::Error(Transport, hyper::Error(Http2, Error { kind: GoAway(b"", FRAME_SIZE_ERROR, Library) }))
2025-06-06T15:12:47.992763Z ERROR  OpenTelemetry trace error occurred: Exporter otlp encountered the following error(s): the grpc server returns error (Unknown error): , detailed error message: h2 protocol error: http2 error tonic::transport::Error(Transport, hyper::Error(Http2, Error { kind: GoAway(b"", FRAME_SIZE_ERROR, Library) }))
```

This is tracked upstream at https://github.com/open-telemetry/opentelemetry-collector/issues/10952.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7595

### Add `graphql.operation.name` attribute to `apollo.router.opened.subscriptions` counter ([PR #7606](https://github.com/apollographql/router/pull/7606))

The `apollo.router.opened.subscriptions` metric has an `graphql.operation.name` attribute applied to identify the named operation of open subscriptions.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7606

## 🛠 Maintenance

### Measure `preview_extended_error_metrics` in Apollo config telemetry ([PR #7597](https://github.com/apollographql/router/pull/7597))

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/7597

## 📚 Documentation

### Document Apollo Runtime Container deployment ([PR #7734](https://github.com/apollographql/router/pull/7734) and [PR #7668](https://github.com/apollographql/router/pull/7668))

The Apollo Runtime Container is now included in our documentation for deployment options.  It also includes instructions for running Apollo Router with the Apollo MCP Server.

By [@jonathanrainer](https://github.com/jonathanrainer) and [@lambertjosh](https://github.com/lambertjosh) in https://github.com/apollographql/router/pull/7734 and https://github.com/apollographql/router/pull/7668

### Fix incorrect reference to `apollo.router.schema.load.duration` ([PR #7582](https://github.com/apollographql/router/pull/7582))

The [in-memory cache documentation](https://www.apollographql.com/docs/graphos/routing/performance/caching/in-memory#cache-warm-up) was referencing an incorrect metric to track schema load times. Previously it was referred to as `apollo.router.schema.loading.time`, whereas the metric being emitted by the router since v2.0.0 is actually `apollo.router.schema.load.duration`. This is now fixed.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/7582

# [2.3.0] - 2025-06-02

## 🚀 Features

**Connectors improvements**: Router 2.3.0 supports Connect spec v0.2, including batch requests, error customization, and direct access to HTTP headers. To use these features: upgrade your Router to 2.3, update your version of Federation to 2.11, and update the @link directives in your subgraphs to https://specs.apollo.dev/connect/v0.2.

See the [Connectors changelog](https://www.apollographql.com/docs/graphos/connectors/reference/changelog) for more details.

### Log whether safe-listing enforcement was skipped ([Issue #7509](https://github.com/apollographql/router/issues/7509))

When logging unknown operations encountered during safe-listing, include information about whether enforcement was skipped. This will help distinguish between truly problematic external operations (where `enforcement_skipped` is false) and internal operations that are intentionally allowed to bypass safelisting (where `enforcement_skipped` is true).

By [@DaleSeo](https://github.com/DaleSeo) in https://github.com/apollographql/router/pull/7509

### Add response body telemetry selector ([PR #7363](https://github.com/apollographql/router/pull/7363))

The Router now supports a `response_body` selector which provides access to the response body in telemetry configurations. This enables more detailed monitoring and logging of response data in the Router.

Example configuration:
```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          "my_attribute":
            response_body: true
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7363

### Support non-JSON and JSON-like content types for connectors ([PR #7380](https://github.com/apollographql/router/pull/7380))

Connectors now inspect the `content-type` header of responses to determine how they should treat the response. This allows more flexibility as prior to this change, all responses were treated as JSON which would lead to errors on non-json responses.

The behavior is as follows:

- If `content-type` ends with `/json` (like `application/json`) OR `+json` (like `application/vnd.foo+json`): content is parsed as JSON.
- If `content-type` is `text/plain`: content will be treated as a UTF-8 `string`. Content can be accessed in `selection` mapping via `$` variable.
- If `content-type` is any other value: content will be treated as a JSON `null`.
- If no `content-type` header is provided: content is assumed to be JSON and therefore parsed as JSON.

If deserialization fails, an error message of `Response deserialization failed` with a error code of `CONNECTOR_DESERIALIZE` will be returned:

```json
"errors": [
    {
        "message": "Response deserialization failed",
        "extensions": {
            "code": "CONNECTOR_DESERIALIZE"
        }
    }
]
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7380

### Include message and path for certain errors in Apollo telemetry ([PR #7378](https://github.com/apollographql/router/pull/7378))

For errors pertaining to connectors and demand control features, Apollo telemetry will now include the original error message and path as part of the traces sent to GraphOS.

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/7378

### Support ignoring specific headers during subscriptions deduplication ([PR #7070](https://github.com/apollographql/router/pull/7070))

The Router now supports ignoring specific headers when deduplicating requests to subgraphs which provide subscription events. Previously, any differing headers which didn't actually affect the subscription response (e.g., `user-agent`) would prevent or limit the potential of deduplication.

The introduction of the `ignored_headers` option allows you to specify headers to ignore during deduplication, enabling you to benefit from subscription deduplication even when requests include headers with unique or varying values that don't affect the subscription's event data.

Configuration example:

```yaml
subscription:
  enabled: true
  deduplication:
    enabled: true # optional, default: true
    ignored_headers: # (optional) List of ignored headers when deduplicating subscriptions
      - x-transaction-id
      - custom-header-name
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7070

## 🐛 Fixes

### Support disabling the health check endpoint ([PR #7519](https://github.com/apollographql/router/pull/7519))

During the development of Router 2.0, the health check endpoint support was converted to be a plugin. Unfortunately, the support for disabling the health check endpoint was lost during the conversion.

This is now fixed and a new unit test ensures that disabling the health check does not result in the creation of a health check endpoint.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7519

### Propagate client name and version modifications through telemetry ([PR #7369](https://github.com/apollographql/router/pull/7369))

The Router accepts modifications to the client name and version (`apollo::telemetry::client_name` and `apollo::telemetry::client_version`), but those modifications were not propagated through the telemetry layers to update spans and traces.

After this change, the modifications from plugins **on the `router` service** are propagated through the telemetry layers.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7369

### Prevent connectors error when using a variable in a nested input argument ([PR #7472](https://github.com/apollographql/router/pull/7472))

The connectors plugin will no longer error when using a variable in a nested input argument. The following example would error prior to this change:

```graphql
query Query (: String){
    complexInputType(filters: { inSpace: true, search:  })
}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7472

### Spans should only include path in `http.route` ([PR #7390](https://github.com/apollographql/router/pull/7390))

Per the [OpenTelemetry spec](https://opentelemetry.io/docs/specs/semconv/attributes-registry/http/#http-route), the `http.route` should only include "the matched route, that is, the path template used in the format used by the respective server framework."

Prior to this change, the Router sends the full URI in `http.route`, which can be high cardinality (ie `/graphql?operation=one_of_many_values`). The Router will now only include the path (`/graphql`).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7390

### Decrease log level for JWT authentication failure ([PR #7396](https://github.com/apollographql/router/pull/7396))

A recent change increased the log level of JWT authentication failures from `info` to `error`. This reverts that change.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7396

### Prefer headers propagated with Router YAML config over headers from Connector directives ([PR #7499](https://github.com/apollographql/router/pull/7499))

When configuring the same header name in both `@connect(http: { headers: })` (or `@source(http: { headers: })`) in SDL and `propagate` in Router YAML configuration, the request had both headers, even if the value is the same. After this change, Router YAML configuration always wins.

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7499

## 🛠 Maintenance

### Add timeouts and connection health checks to Redis connections ([Issue #6855](https://github.com/apollographql/router/issues/6855))

The Router's internal Redis configuration has been improved to increase client resiliency under various failure modes (TCP failures and timeouts, unresponsive sockets, Redis server failures, etc.). It also adds heartbeats (a PING every 10 seconds) to the Redis clients.

By [@aembke](https://github.com/aembke), [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7526

## 📚 Documentation

### Fix discrepancies in coprocessor metrics documentation ([PR #7359](https://github.com/apollographql/router/pull/7359))

The documentation for standard metric instruments for [coprocessors](https://www.apollographql.com/docs/graphos/routing/observability/telemetry/instrumentation/standard-instruments#coprocessor) has been updated:

- Rename `apollo.router.operations.coprocessor.total` to `apollo.router.operations.coprocessor`
- Clarify that `coprocessor.succeeded` attribute applies to `apollo.router.operations.coprocessor` only.

By [@shorgi](https://github.com/shorgi) in https://github.com/apollographql/router/pull/7359

### Add example Rhai script for returning Demand Control metrics as response headers ([PR #7564](https://github.com/apollographql/router/pull/7564))

A new section has been added to the [demand control documentation](https://www.apollographql.com/docs/graphos/routing/security/demand-control#accessing-programmatically) to demonstrate how to use Rhai scripts to expose cost estimation data in response headers. This allows clients to see the estimated cost, actual cost, and other demand control metrics directly in HTTP responses, which is useful for debugging and client-side optimization.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/7564



# [2.2.1] - 2025-05-13

## 🐛 Fixes

### Redis connection leak on schema changes ([PR #7319](https://github.com/apollographql/router/pull/7319))

The router performs a 'hot reload' whenever it detects a schema update. During this reload, it effectively instantiates a new internal router, warms it up (optional), redirects all traffic to this new router, and drops the old internal router.

This change fixes a bug in that "drop" process where the Redis connections are never told to terminate, even though the Redis client pool is dropped. This leads to an ever-increasing number of inactive Redis connections as each new schema comes in and goes out of service, which eats up memory.

The solution adds a new up-down counter metric, `apollo.router.cache.redis.connections`, to track the number of open Redis connections. This metric includes a `kind` label to discriminate between different Redis connection pools, which mirrors the `kind` label on other cache metrics (ie `apollo.router.cache.hit.time`).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7319

### Propagate client name and version modifications through telemetry ([PR #7369](https://github.com/apollographql/router/pull/7369))

The router accepts modifications to the client name and version (`apollo::telemetry::client_name` and `apollo::telemetry::client_version`), but those modifications are not currently propagated through the telemetry layers to update spans and traces.

This PR moves where the client name and version are bound to the span, so that the modifications from plugins **on the `router` service** are propagated.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7369

### Progressive overrides are not disabled when connectors are used ([PR #7351](https://github.com/apollographql/router/pull/7351))

Prior to this fix, introducing a connector disabled the progressive override plugin.

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/7351

### Avoid unnecessary cloning in the deduplication plugin ([PR #7347](https://github.com/apollographql/router/pull/7347))

The deduplication plugin always cloned responses, even if there were not multiple simultaneous requests that would benefit from the cloned response.

We now check to see if deduplication will provide a benefit before we clone the subgraph response.

There was also an undiagnosed race condition which meant that a notification could be missed. This would have resulted in additional work being performed as the missed notification would have led to another subgraph request.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7347

### Spans should only include path in `http.route` ([PR #7390](https://github.com/apollographql/router/pull/7390))

Per the [OpenTelemetry spec](https://opentelemetry.io/docs/specs/semconv/attributes-registry/http/#http-route), the `http.route` should only include "the matched route, that is, the path template used in the format used by the respective server framework."

The router currently sends the full URI in `http.route`, which can be high cardinality (ie `/graphql?operation=one_of_many_values`). After this change, the router will only include the path (`/graphql`).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7390

### Decrease log level for JWT authentication failure ([PR #7396](https://github.com/apollographql/router/pull/7396))

A recent change inadvertently increased the log level of JWT authentication failures from `info` to `error`. This reverts that change returning it to the previous behavior.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7396

### Avoid fractional decimals when generating `apollo.router.operations.batching.size` metrics for GraphQL request batch sizes ([PR #7306](https://github.com/apollographql/router/pull/7306))

Corrects the calculation of the `apollo.router.operations.batching.size` metric to reflect accurate batch sizes rather than occasionally returning fractional numbers.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7306

## 📃 Configuration

### Log warnings for deprecated coprocessor `context` configuration usage ([PR #7349](https://github.com/apollographql/router/pull/7349))

`context: true` is an alias for `context: deprecated` but should not be used. The router now logs a runtime warning on startup if you do use it.

Instead of:

```yaml
coprocessor:
  supergraph:
    request:
      context: true # ❌
```

Explicitly use `deprecated` or `all`:

```yaml
coprocessor:
  supergraph:
    request:
      context: deprecated # ✅
```

See [the 2.x upgrade guide](https://www.apollographql.com/docs/graphos/routing/upgrade/from-router-v1#context-keys-for-coprocessors) for more detailed upgrade steps.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7349

## 🛠 Maintenance

### Linux: Compatibility with glibc 2.28 or newer ([PR #7355](https://github.com/apollographql/router/pull/7355))

The default build images provided in our CI environment have a relatively modern version of `glibc` (2.35). This means that on some distributions, notably those based around RedHat, it wasn't possible to use our binaries since the version of `glibc` was older than 2.35.

We now maintain a build image which is based on a distribution with `glibc` 2.28. This is old enough that recent releases of either of the main Linux distribution families (Debian and RedHat) can make use of our binary releases.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7355

### Reject `@skip`/`@include` on subscription root fields in validation ([PR #7338](https://github.com/apollographql/router/pull/7338))

This implements a [GraphQL spec RFC](https://github.com/graphql/graphql-spec/pull/860), rejecting subscriptions in validation that can be invalid during execution.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7338

## 📚 Documentation

### Query planning best practices ([PR #7263](https://github.com/apollographql/router/pull/7263))

Added a new page under Routing docs about [Query Planning Best Practices](https://www.apollographql.com/docs/graphos/routing/query-planning/query-planning-best-practices).

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/7263

# [2.2.0] - 2025-04-28

## 🚀 Features

### Add support for connector header propagation via YAML config ([PR #7152](https://github.com/apollographql/router/pull/7152))

Added support for connector header propagation via YAML config. All of the existing header propagation in the Router now works for connectors by using
`headers.connector.all` to apply rules to all connectors or `headers.connector.sources.*` to apply rules to specific sources.

Note that if one of these rules conflicts with a header set in your schema, either in `@connect` or `@source`, the value in your Router config will
take priority and be treated as an override.

```yaml
headers:
  connector:
    all: # configuration for all connectors across all subgraphs
      request:
        - insert:
            name: "x-inserted-header"
            value: "hello world!"
        - propagate:
            named: "x-client-header"
    sources:
      connector-graph.random_person_api:
        request:
          - insert:
              name: "x-inserted-header"
              value: "hello world!"
          - propagate:
              named: "x-client-header"
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7152

### Enable configuration auto-migration for minor version bumps ([PR #7162](https://github.com/apollographql/router/pull/7162))

To facilitate configuration evolution within major versions of the router's lifecycles (e.g., within 2.x.x versions), YAML configuration migrations are applied automatically. To avoid configuration drift and facilitate maintenance, when upgrading to a new major version the migrations from the previous major (e.g., 1.x.x) will not be applied automatically. These will need to be applied with `router config upgrade` prior to the upgrade.  To facilitate major version upgrades, we recommend regularly applying the configuration changes using `router config upgrade` and committing those to your version control system.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7162

### Allow expressions in more locations in Connectors URIs ([PR #7220](https://github.com/apollographql/router/pull/7220))

Previously, we only allowed expressions in very specific locations in Connectors URIs:

1. A path segment, like `/users/{$args.id}`
2. A query parameter's _value_, like `/users?id={$args.id}`

Expressions can now be used anywhere in or after the path of the URI.
For example, you can do
`@connect(http: {GET: "/users?{$args.filterName}={$args.filterValue}"})`.
The result of any expression will _always_ be percent encoded.

> Note: Parts of this feature are only available when composing with Apollo Federation v2.11 or above (currently in preview).

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220

### Enables reporting of persisted query usage by PQ ID to Apollo ([PR #7166](https://github.com/apollographql/router/pull/7166))

This change allows the router to report usage metrics by persisted query ID to Apollo, so that we can show usage stats for PQs.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/7166

### Instrument coprocessor request with `http_request` span ([Issue #6739](https://github.com/apollographql/router/issues/6739))

Coprocessor requests will now emit an `http_request` span. This span can help to gain
insight into latency that may be introduced over the network stack when communicating with coprocessor.

Coprocessor span attributes are:

- `otel.kind`: `CLIENT`
- `http.request.method`: `POST`
- `server.address`: `<target address>`
- `server.port`: `<target port>`
- `url.full`: `<url.full>`
- `otel.name`: `<method> <url.full>`
- `otel.original_name`: `http_request`

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/6776

### Enables reporting for client libraries that send the library name and version information in operation requests. ([PR #7264](https://github.com/apollographql/router/pull/7264))

Apollo client libraries can send the library name and version information in the `extensions` key of an operation request. If those values are found in a request the router will include them in the telemetry operation report sent to Apollo.

By [@calvincestari](https://github.com/calvincestari) in https://github.com/apollographql/router/pull/7264

### Add compute job pool spans ([PR #7236](https://github.com/apollographql/router/pull/7236))

The compute job pool in the router is used to execute CPU intensive work outside of the main I/O worker threads, including GraphQL parsing, query planning, and introspection.
This PR adds spans to jobs that are on this pool to allow users to see when latency is introduced due to
resource contention within the compute job pool.

* `compute_job`:
  - `job.type`: (`query_parsing`|`query_planning`|`introspection`)
* `compute_job.execution`
  - `job.age`: `P1`-`P8`
  - `job.type`: (`query_parsing`|`query_planning`|`introspection`)

Jobs are executed highest priority (`P8`) first. Jobs that are low priority (`P1`) age over time, eventually executing
at highest priority. The age of a job is can be used to diagnose if a job was waiting in the queue due to other higher
priority jobs also in the queue.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7236

### JWT authorization supports multiple issuers ([Issue #6172](https://github.com/apollographql/router/issues/6172))

Allow JWT authorization options to support multiple issuers using the same JWKS.

**Configuration change**: any `issuer` defined on currently existing `authentication.router.jwt.jwks` needs to be
migrated to an entry in the `issuers` list.  This configuration will happen automatically until the next major version of the router. This change can be committed using `./router config upgrade` prior to the next major release.

For example, the following configuration:

```yaml
authentication:
  router:
    jwt:
      jwks:
        - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
          issuer: https://issuer.one
```

Will be changed to contain an array of `issuers` rather than a single `issuer`:

```yaml
authentication:
  router:
    jwt:
      jwks:
        - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
          issuers:
            - https://issuer.one
            - https://issuer.two
```

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/7170

## 🐛 Fixes

### Fix JWT metrics discrepancy ([PR #7258](https://github.com/apollographql/router/pull/7258))

This fixes the `apollo.router.operations.authentication.jwt` counter metric to behave [as documented](https://www.apollographql.com/docs/graphos/routing/security/jwt#observability): emitted for every request that uses JWT, with the `authentication.jwt.failed` attribute set to true or false for failed or successful authentication.

Previously, it was only used for failed authentication.

The attribute-less and accidentally-differently-named `apollo.router.operations.jwt` counter was and is only emitted for successful authentication, but is deprecated now.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/7258

### Fix potential telemetry deadlock ([PR #7142](https://github.com/apollographql/router/pull/7142))

The `tracing_subscriber` crate uses `RwLock`s to manage access to a `Span`'s `Extensions`. Deadlocks are possible when
multiple threads access this lock, including with reentrant locks:
```
// Thread 1              |  // Thread 2
let _rg1 = lock.read();  |
                         |  // will block
                         |  let _wg = lock.write();
// may deadlock          |
let _rg2 = lock.read();  |
```

This fix removes an opportunity for reentrant locking while extracting a Datadog identifier.

There is also a potential for deadlocks when the root and active spans' `Extensions` are acquired at the same time, if
multiple threads are attempting to access those `Extensions` but in a different order. This fix removes a few cases
where multiple spans' `Extensions` are acquired at the same time.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7142

### Check if JWT claim is part of the context before getting the JWT expiration with subscriptions ([PR #7069](https://github.com/apollographql/router/pull/7069))

In v2.1.0 we introduced [logs](https://github.com/apollographql/router/pull/6930/files#diff-7597092ab9d509e0ffcb328691f1dded20f69d849f142628095f0455aa49880cR648) for the `jwt_expires_in` function which caused an unexpectedly chatty logging when using subscriptions.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7069

### Parse nested input types and report them ([PR #6900](https://github.com/apollographql/router/pull/6900))

Fixes a bug where enums that were arguments to nested queries were not being reported.

By [@merylc](https://github.com/merylc) in https://github.com/apollographql/router/pull/6900

### Add compute job pool metrics ([PR #7184](https://github.com/apollographql/router/pull/7184))

The compute job pool is used within the router for compute intensive jobs that should not block the Tokio worker threads.
When this pool becomes saturated it is difficult for users to see why so that they can take action.
This change adds new metrics to help users understand how long jobs are waiting to be processed.

New metrics:
- `apollo.router.compute_jobs.queue_is_full` - A counter of requests rejected because the queue was full.
- `apollo.router.compute_jobs.duration` - A histogram of time spent in the compute pipeline by the job, including the queue and query planning.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)
  - `job.outcome`: (`executed_ok`, `executed_error`, `channel_error`, `rejected_queue_full`, `abandoned`)
- `apollo.router.compute_jobs.queue.wait.duration` - A histogram of time spent in the compute queue by the job.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)
- `apollo.router.compute_jobs.execution.duration` - A histogram of time spent to execute job (excludes time spent in the queue).
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)
- `apollo.router.compute_jobs.active_jobs` - A gauge of the number of compute jobs being processed in parallel.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7184

### Preserve trailing slashes in Connectors URIs ([PR #7220](https://github.com/apollographql/router/pull/7220))

Previously, a URI like `@connect(http: {GET: "/users/"})` could be normalized to `@connect(http: {GET: "/users"})`. This
change preserves the trailing slash, which is significant to some web servers.

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220

### Support @context/@fromContext when using Connectors ([PR #7132](https://github.com/apollographql/router/pull/7132))

This fixes a bug that dropped the `@context` and `@fromContext` directives when introducing a connector.

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/7132

### telemetry: correctly apply conditions on events ([PR #7325](https://github.com/apollographql/router/pull/7325))

Fixed a issue where conditional telemetry events weren't being properly evaluated.
This affected both standard events (`response`, `error`) and custom telemetry events.

For example in config like this:
```yaml
telemetry:
  instrumentation:
    events:
      supergraph:
        request:
          level: info
          condition:
            eq:
            - request_header: apollo-router-log-request
            - testing
        response:
          level: info
          condition:
            eq:
            - request_header: apollo-router-log-request
            - testing
```

The Router would emit the `request` event when the header matched, but never emit the `response` event - even with the same matching header.

This fix ensures that all event conditions are properly evaluated, restoring expected telemetry behavior and making conditional logging work correctly throughout the entire request lifecycle.

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/7325

### Connection shutdown timeout 1.x ([PR #7058](https://github.com/apollographql/router/pull/7058))

When a connection is closed we call `graceful_shutdown` on hyper and then await for the connection to close.

Hyper 0.x has various issues around shutdown that may result in us waiting for extended periods for the connection to eventually be closed.

This PR introduces a configurable timeout from the termination signal to actual termination, defaulted to 60 seconds. The connection is forcibly terminated after the timeout is reached.

To configure, set the option in router yaml. It accepts human time durations:
```
supergraph:
  connection_shutdown_timeout: 60s
```

Note that even after connections have been terminated the router will still hang onto pipelines if `early_cancel` has not been configured to true. The router is trying to complete the request.

Users can either set `early_cancel` to `true`
```
supergraph:
  early_cancel: true
```

AND/OR use traffic shaping timeouts:
```
traffic_shaping:
  router:
    timeout: 60s
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7058

###  Clarify tracing error messages in coprocessor's stages (PR #6791)

Trace messages in coprocessors used `external extensibility` namespace. They now use `coprocessor` in the message instead for clarity.

By [@briannafugate408](https://github.com/briannafugate408)

### Fix crash when an invalid query plan is generated ([PR #7214](https://github.com/apollographql/router/pull/7214))

When an invalid query plan is generated, the router could panic and crash.
This could happen if there are gaps in the GraphQL validation implementation.
Now, even if there are unresolved gaps, the router will handle it gracefully and reject the request.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7214

### Fix Apollo request metadata generation for errors ([PR #7021](https://github.com/apollographql/router/pull/7021))

* Fixes the Apollo operation ID and name generated for requests that fail due to parse, validation, or invalid operation name errors.
* Updates the error code generated for operations with an invalid operation name from GRAPHQL_VALIDATION_FAILED to GRAPHQL_UNKNOWN_OPERATION_NAME

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/7021

### Enable Integer Error Code Reporting ([PR #7226](https://github.com/apollographql/router/pull/7226))

Fixes an issue where numeric error codes (e.g. 400, 500) were not properly parsed into a string and thus were not
reported to Apollo error telemetry.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/7226

### Increase compute job pool queue size ([PR #7205](https://github.com/apollographql/router/pull/7205))

The compute job pool in the router is used to execute CPU intensive work outside of the main I/O worker threads, including GraphQL parsing, query planning, and introspection. When the pool is busy, jobs enter a queue.

We previously set this queue size to 20 (per thread). However, this may be too small on resource constrained environments.

This patch increases the queue size to 1,000 jobs per thread. For reference, in older router versions before the introduction of the compute job worker pool, the equivalent queue size was *1,000*.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7205

### Relax percent encoding for Connectors ([PR #7220](https://github.com/apollographql/router/pull/7220))

Characters outside of `{ }` expressions will no longer be percent encoded unless they are completely invalid for a
URI. For example, in an expression like `@connect(http: {GET: "/products?filters[category]={$args.category}"})` the
square
braces `[ ]` will no longer be percent encoded. Any string from within a dynamic `{ }` will still be percent encoded.

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220

### Preserve `data: null` when handling coprocessor GraphQL responses which included `errors` ([PR #7141](https://github.com/apollographql/router/pull/7141))

Previously, Router incorrectly swallowed `data: null` conditions on GraphQL responses returned from a coprocessor.

According to [GraphQL Spectification](https://spec.graphql.org/draft/#sel-FAPHLJCAACEBxlY):

> If an error was raised during the execution that prevented a valid response, the "data" entry in the response **should be null**.

That means if coprocessor returned a valid execution error, for example:

```json
{
  "data": null,
  "errors": [{ "message": "Some execution error" }]
}
```

It was incorrect (and inadvertent) to return the following response to the client:

```json
{
  "errors": [{ "message": "Some execution error" }]
}
```

This fix ensures compliance with the GraphQL specification in this regard by preserving the complete structure of the response returned from coprocessors.

Contributed by [@IvanGoncharov](https://github.com/IvanGoncharov) in [#7141](https://github.com/apollographql/router/pull/7141)

### Helm: Correct default telemetry `resource` property in `ConfigMap` ([Issue #6104](https://github.com/apollographql/router/issues/6104))

The Helm chart was using an outdated value when emitting the `telemetry.exporters.metrics.common.resource.service.name` values.  This has been updated to use the correct (singular) version of `resource` (rather than the incorrect `resources` which was used earlier in 1.x's life-cycle).

By [@vatsalpatel](https://github.com/vatsalpatel) in https://github.com/apollographql/router/pull/6105

### Update Dockerfile exec script to use `#!/bin/bash` instead of `#!/usr/bin/env bash` ([Issue #3517](https://github.com/apollographql/router/issues/3517))

For users of Google Cloud Platform (GCP) Cloud Run platform, using the router's default Docker image was not possible due to an error that would occur during startup:

```sh
"/usr/bin/env: 'bash ': No such file or directory"
```

To avoid this issue, we've changed the script to use `#!/bin/bash` instead of `#!/usr/bin/env bash`, as we use a fixed Linux distribution in Docker which has the Bash binary located in a fixed location.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/7198

### Remove "setting resource attributes is not allowed" warning ([PR #7272](https://github.com/apollographql/router/pull/7272))

If Uplink was enabled, Router 2.1.x emitted this warning at startup even when there was no user configuration responsible for the condition:

```
WARN  setting resource attributes is not allowed for Apollo telemetry
```

The warning is removed entirely.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/7272

## 📃 Configuration

### Customization of "header read timeout" ([PR #7262](https://github.com/apollographql/router/pull/7262))

This change exposes the server's header read timeout as the `server.http.header_read_timeout` configuration option.

By default, the `server.http.header_read_timeout` is set to previously hard-coded 10 seconds. A longer timeout can be configured using the `server.http.header_read_timeout` option.

```yaml title="router.yaml"
server:
  http:
    header_read_timeout: 30s
```

By [@gwardwell ](https://github.com/gwardwell) in https://github.com/apollographql/router/pull/7262

### Fine-grained control over `include_subgraph_errors` ([Issue #6402](https://github.com/apollographql/router/pull/6402)

Update `include_subgraph_errors` with additional configuration options for both global and subgraph levels. This update provides finer control over error messages and extension keys for each subgraph.
For more details, please read [subgraph error inclusion](https://www.apollographql.com/docs/graphos/routing/observability/subgraph-error-inclusion).

```yaml
include_subgraph_errors:
  all:
    redact_message: true
    allow_extensions_keys:
      - code
  subgraphs:
    product:
      redact_message: false  # Propagate original error messages
      allow_extensions_keys: # Extend global allow list - `code` and `reason` will be propagated
        - reason
      exclude_global_keys:   # Exclude `code` from global allow list - only `reason` will be propagated.
        - code
    account:
      deny_extensions_keys:  # Overrides global allow list
        - classification
    review: false            # Redact everything.

    # Undefined subgraphs inherits default global settings from `all`
```

**Note:** Using a `deny_extensions_keys` approach carries security risks because any sensitive information not explicitly included in the deny list will be exposed to clients. For better security, subgraphs should prefer to redact everything or `allow_extensions_keys` when possible.

By [@Samjin](https://github.com/Samjin) and [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7164

### Add new configurable delivery pathway for high cardinality GraphOS Studio metrics ([PR #7138](https://github.com/apollographql/router/pull/7138))

This change provides a secondary pathway for new "realtime" GraphOS Studio metrics whose delivery interval is configurable due to their higher cardinality. These metrics will respect `telemetry.apollo.batch_processor.scheduled_delay` as configured on the realtime path.  All other Apollo metrics will maintain the previous hardcoded 60s send interval.

By [@rregitsky](https://github.com/rregitsky) and [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/7138

## 📚 Documentation

### GraphQL error codes that can occur during router execution ([PR #7160](https://github.com/apollographql/router/issues/7160))

Added documentation for more GraphQL error codes that can occur during router execution, including better differentiation between HTTP status codes and GraphQL error extensions codes.

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/7160

### Update API Gateway tech note ([PR #7261](https://github.com/apollographql/router/pull/7261))

Update the [Router vs Gateway Tech Note](https://www.apollographql.com/docs/graphos/routing/router-api-gateway-comparison) with more details now that we have connectors

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/7261

### Extended errors preview configuration ([PR 7038](https://github.com/apollographql/router/pull/7038))

We've introduced documentation for [GraphOS extended error reporting](https://www.apollographql.com/docs/graphos/routing/configuration#extended-error-reporting).

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/7038

### Add tip about `Apollo-Expose-Query-Plan: dry-run` to Cache warm-up ([PR #6973](https://github.com/apollographql/router/pull/6973))

The [Cache warm-up documentation](https://www.apollographql.com/docs/graphos/routing/performance/caching/in-memory#cache-warm-up) now flags the availability of the `Apollo-Expose-Query-Plan: dry-run` header.

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/6973

# [2.1.3] - 2025-04-16

## 🐛 Fixes

### Entity-cache: handle multiple key directives ([PR #7228](https://github.com/apollographql/router/pull/7228))

This PR fixes a bug in entity caching introduced by the fix in https://github.com/apollographql/router/pull/6888 for cases where several `@key` directives with different fields were declared on a type as documented [here](https://www.apollographql.com/docs/graphos/schema-design/federated-schemas/reference/directives#managing-types).

For example if you have this kind of entity in your schema:

```graphql
type Product @key(fields: "upc") @key(fields: "sku") {
  upc: ID!
  sku: ID!
  name: String
}
```

By [@duckki](https://github.com/duckki) & [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7228

### Improve Error Message for Invalid JWT Header Values ([PR #7121](https://github.com/apollographql/router/pull/7121))

Enhanced parsing error messages for JWT Authorization header values now provide developers with clear, actionable feedback while ensuring that no sensitive data is exposed.

Examples of the updated error messages:
```diff
-         Header Value: '<invalid value>' is not correctly formatted. prefix should be 'Bearer'
+         Value of 'authorization' JWT header should be prefixed with 'Bearer'
```

```diff
-         Header Value: 'Bearer' is not correctly formatted. Missing JWT
+         Value of 'authorization' JWT header has only 'Bearer' prefix but no JWT token
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/7121

### Fix crash when an invalid query plan is generated ([PR #7214](https://github.com/apollographql/router/pull/7214))

When an invalid query plan is generated, the router could panic and crash.
This could happen if there are gaps in the GraphQL validation implementation.
Now, even if there are unresolved gaps, the router will handle it gracefully and reject the request.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7214

# [2.1.2] - 2025-04-14

## 🐛 Fixes

### Support `@context`/`@fromContext` when using Connectors ([PR #7132](https://github.com/apollographql/router/pull/7132))

This fixes a bug that dropped the `@context` and `@fromContext` directives when introducing a connector.

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/7132

## 📃 Configuration

### Add new configurable delivery pathway for high cardinality Apollo Studio metrics ([PR #7138](https://github.com/apollographql/router/pull/7138))

This change provides a secondary pathway for new "realtime" Studio metrics whose delivery interval is configurable due to their higher cardinality. These metrics will respect `telemetry.apollo.batch_processor.scheduled_delay` as configured on the realtime path.

All other Apollo metrics will maintain the previous hardcoded 60s send interval.

By [@rregitsky](https://github.com/rregitsky) and [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/7138



# [2.1.1] - 2025-04-07

## 🔒 Security

### Certain query patterns may cause resource exhaustion

Corrects a set of denial-of-service (DOS) vulnerabilities that made it possible for an attacker to render router inoperable with certain simple query patterns due to uncontrolled resource consumption. All prior-released versions and configurations are vulnerable except those where `persisted_queries.enabled`, `persisted_queries.safelist.enabled`, and `persisted_queries.safelist.require_id` are all `true`.

See the associated GitHub Advisories [GHSA-3j43-9v8v-cp3f](https://github.com/apollographql/router/security/advisories/GHSA-3j43-9v8v-cp3f), [GHSA-84m6-5m72-45fp](https://github.com/apollographql/router/security/advisories/GHSA-84m6-5m72-45fp), [GHSA-75m2-jhh5-j5g2](https://github.com/apollographql/router/security/advisories/GHSA-75m2-jhh5-j5g2), and [GHSA-94hh-jmq8-2fgp](https://github.com/apollographql/router/security/advisories/GHSA-94hh-jmq8-2fgp), and the `apollo-compiler` GitHub Advisory [GHSA-7mpv-9xg6-5r79](https://github.com/apollographql/apollo-rs/security/advisories/GHSA-7mpv-9xg6-5r79) for more information.

By [@sachindshinde](https://github.com/sachindshinde) and [@goto-bus-stop](https://github.com/goto-bus-stop).

# [2.1.0] - 2025-03-25

## 🚀 Features

### Connectors: support for traffic shaping ([PR #6737](https://github.com/apollographql/router/pull/6737))

Traffic shaping is now supported for connectors. To target a specific source, use the `subgraph_name.source_name` under the new `connector.sources` property of `traffic_shaping`. Settings under `connector.all` will apply to all connectors. `deduplicate_query` is not supported at this time.

Example config:

```yaml
traffic_shaping:
  connector:
    all:
      timeout: 5s
    sources:
      connector-graph.random_person_api:
        global_rate_limit:
          capacity: 20
          interval: 1s
        experimental_http2: http2only
        timeout: 1s
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6737

### Connectors: Support TLS configuration ([PR #6995](https://github.com/apollographql/router/pull/6995))

Connectors now supports TLS configuration for using custom certificate authorities and utilizing client certificate authentication.

```yaml
tls:
  connector:
    sources:
      connector-graph.random_person_api:
        certificate_authorities: ${file.ca.crt}
        client_authentication:
          certificate_chain: ${file.client.crt}
          key: ${file.client.key}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6995

### Update JWT handling ([PR #6930](https://github.com/apollographql/router/pull/6930))

This PR updates JWT-handling in the `AuthenticationPlugin`;

- Users may now set a new config option `config.authentication.router.jwt.on_error`.
  - When set to the default `Error`, JWT-related errors will be returned to users (the current behavior).
  - When set to `Continue`, JWT errors will instead be ignored, and JWT claims will not be set in the request context.
- When JWTs are processed, whether processing succeeds or fails, the request context will contain a new variable `apollo::authentication::jwt_status` which notes the result of processing.

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/6930

### Add `batching.maximum_size` configuration option to limit maximum client batch size ([PR #7005](https://github.com/apollographql/router/pull/7005))

Add an optional `maximum_size` parameter to the batching configuration.

* When specified, the router will reject requests which contain more than `maximum_size` queries in the client batch.
* When unspecified, the router performs no size checking (the current behavior).

If the number of queries provided exceeds the maximum batch size, the entire batch fails with error code 422 (`Unprocessable Content`). For example:

```json
{
  "errors": [
    {
      "message": "Invalid GraphQL request",
      "extensions": {
        "details": "Batch limits exceeded: you provided a batch with 3 entries, but the configured maximum router batch size is 2",
        "code": "BATCH_LIMIT_EXCEEDED"
      }
    }
  ]
}
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7005

### Introduce PQ manifest `hot_reload` option for local manifests ([PR #6987](https://github.com/apollographql/router/pull/6987))

This change introduces a [`persisted_queries.hot_reload` configuration option](https://www.apollographql.com/docs/graphos/routing/security/persisted-queries#hot_reload) to allow the router to hot reload local PQ manifest changes.

If you configure `local_manifests`, you can set `hot_reload` to `true` to automatically reload manifest files whenever they change. This lets you update local manifest files without restarting the router.

```yaml
persisted_queries:
  enabled: true
  local_manifests:
    - ./path/to/persisted-query-manifest.json
  hot_reload: true
```

Note: This change explicitly does _not_ piggyback on the existing `--hot-reload` flag.

By [@trevor-scheer](https://github.com/trevor-scheer) in https://github.com/apollographql/router/pull/6987

### Add support to get/set URI scheme in Rhai ([Issue #6897](https://github.com/apollographql/router/issues/6897))

This adds support to read and write the scheme from the `request.uri.scheme`/`request.subgraph.uri.scheme` functions in Rhai,
enabling the ability to switch between `http` and `https` for subgraph fetches. For example:

```rs
fn subgraph_service(service, subgraph){
    service.map_request(|request|{
        log_info(`${request.subgraph.uri.scheme}`);
        if request.subgraph.uri.scheme == {} {
            log_info("Scheme is not explicitly set");
        }
        request.subgraph.uri.scheme = "https"
        request.subgraph.uri.host = "api.apollographql.com";
        request.subgraph.uri.path = "/api/graphql";
        request.subgraph.uri.port = 1234;
        log_info(`${request.subgraph.uri}`);
    });
}
```
By [@starJammer](https://github.com/starJammer) in https://github.com/apollographql/router/pull/6906

### Add `router config validate` subcommand ([PR #7016](https://github.com/apollographql/router/pull/7016))

Adds new `router config validate` subcommand to allow validation of a router config file without fully starting up the Router.

```
./router config validate <path-to-config-file.yaml>
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7016

### Enable remote proxy downloads of the Router

This enables users without direct download access to specify a remote proxy mirror location for the GitHub download of
the Apollo Router releases.

By [@LongLiveCHIEF](https://github.com/LongLiveCHIEF) in https://github.com/apollographql/router/pull/6667

### Add metric to measure cardinality overflow frequency ([PR #6998](https://github.com/apollographql/router/pull/6998))

Adds a new counter metric, `apollo.router.telemetry.metrics.cardinality_overflow`, that is incremented when the [cardinality overflow log](https://github.com/open-telemetry/opentelemetry-rust/blob/d583695d30681ee1bd910156de27d91be3711822/opentelemetry-sdk/src/metrics/internal/mod.rs#L134) from [opentelemetry-rust](https://github.com/open-telemetry/opentelemetry-rust) occurs. This log means that a metric in a batch has reached a cardinality of > 2000 and that any excess attributes will be ignored.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/6998

### Add metrics for value completion errors ([PR #6905](https://github.com/apollographql/router/pull/6905))

When the router encounters a value completion error, it is not included in the GraphQL errors array, making it harder to observe. To surface this issue in a more obvious way, router now counts value completion error metrics via the metric instruments `apollo.router.graphql.error` and `apollo.router.operations.error`, distinguishable via the `code` attribute with value `RESPONSE_VALIDATION_FAILED`.

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/6905

### Add `apollo.router.pipelines` metrics ([PR #6967](https://github.com/apollographql/router/pull/6967))

When the router reloads, either via schema change or config change, a new request pipeline is created.
Existing request pipelines are closed once their requests finish. However, this may not happen if there are ongoing long requests that do not finish, such as Subscriptions.

To enable debugging when request pipelines are being kept around, a new gauge metric has been added:

- `apollo.router.pipelines` - The number of request pipelines active in the router
    - `schema.id` - The Apollo Studio schema hash associated with the pipeline.
    - `launch.id` - The Apollo Studio launch id associated with the pipeline (optional).
    - `config.hash` - The hash of the configuration

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6967

### Add `apollo.router.open_connections` metric ([PR #7023](https://github.com/apollographql/router/pull/7023))

To help users to diagnose when connections are keeping pipelines hanging around, the following metric has been added:
- `apollo.router.open_connections` - The number of request pipelines active in the router
    - `schema.id` - The Apollo Studio schema hash associated with the pipeline.
    - `launch.id` - The Apollo Studio launch id associated with the pipeline (optional).
    - `config.hash` - The hash of the configuration.
    - `server.address` - The address that the router is listening on.
    - `server.port` - The port that the router is listening on if not a unix socket.
    - `http.connection.state` - Either `active` or `terminating`.

You can use this metric to monitor when connections are open via long running requests or keepalive messages.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7023

### Add span events to error spans for connectors and demand control plugin ([PR #6727](https://github.com/apollographql/router/pull/6727))

New span events have been added to trace spans which include errors. These span events include the GraphQL error code that relates to the error. So far, this only includes errors generated by connectors and the demand control plugin.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/6727

### Changes to experimental error metrics ([PR #6966](https://github.com/apollographql/router/pull/6966))

In 2.0.0, an experimental metric `telemetry.apollo.errors.experimental_otlp_error_metrics` was introduced to track errors with additional attributes. A few related changes are included here:

- Sending these metrics now also respects the subgraph's `send` flag e.g. `telemetry.apollo.errors.subgraph.[all|(subgraph name)].send`.
- A new configuration option `telemetry.apollo.errors.subgraph.[all|(subgraph name)].redaction_policy` has been added. This flag only applies when `redact` is set to `true`. When set to `ErrorRedactionPolicy.Strict`, error redaction will behave as it has in the past. Setting this to `ErrorRedactionPolicy.Extended` will allow the `extensions.code` value from subgraph errors to pass through redaction and be sent to Studio.
- A warning about incompatibility of error telemetry with connectors will be suppressed when this feature is enabled, since it _does_ support connectors when using the new mode.

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/6966


## 🐛 Fixes

### Export gauge instruments ([Issue #6859](https://github.com/apollographql/router/issues/6859))

Previously in router 2.x, when using the router's OTel `meter_provider()` to report metrics from Rust plugins, gauge instruments such as those created using `.u64_gauge()` weren't exported. The router now exports these instruments.

By [@yanns](https://github.com/yanns) in https://github.com/apollographql/router/pull/6865

### Use `batch_processor` config for Apollo metrics `PeriodicReader` ([PR #7024](https://github.com/apollographql/router/pull/7024))

The Apollo OTLP `batch_processor` configurations `telemetry.apollo.batch_processor.scheduled_delay` and `telemetry.apollo.batch_processor.max_export_timeout` now also control the Apollo OTLP `PeriodicReader` export interval and timeout, respectively. This update brings parity between Apollo OTLP metrics and [non-Apollo OTLP exporter metrics](https://github.com/apollographql/router/blob/0f88850e0b164d12c14b1f05b0043076f21a3b28/apollo-router/src/plugins/telemetry/metrics/otlp.rs#L37-L40).

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/7024

### Reduce Brotli encoding compression level ([Issue #6857](https://github.com/apollographql/router/issues/6857))

The Brotli encoding compression level has been changed from `11` to `4` to improve performance and mimic other compression algorithms' `fast` setting. This value is also a much more reasonable value for dynamic workloads.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7007

### CPU count inference improvements for `cgroup` environments ([PR #6787](https://github.com/apollographql/router/pull/6787))

This fixes an issue where the `fleet_detector` plugin would not correctly infer the CPU limits for a system which used `cgroup` or `cgroup2`.

By [@nmoutschen](https://github.com/nmoutschen) in https://github.com/apollographql/router/pull/6787

### Separate entity keys and representation variables in entity cache key ([Issue #6673](https://github.com/apollographql/router/issues/6673))

This fix separates the entity keys and representation variable values in the cache key, to avoid issues with `@requires` for example.

> [!IMPORTANT]
>
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release contains changes which necessarily alter the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6888

### Replace Rhai-specific hot-reload functionality with general hot-reload ([PR #6950](https://github.com/apollographql/router/pull/6950))

In Router 2.0 the rhai hot-reload capability was not working. This was because of architectural improvements to the router which meant that the entire service stack was no longer re-created for each request.

The fix adds the rhai source files into the primary list of elements, configuration, schema, etc..., watched by the router and removes the old Rhai-specific file watching logic.

If --hot-reload is enabled, the router will reload on changes to Rhai source code just like it would for changes to configuration, for example.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6950

## 📃 Configuration

### Make experimental OTLP error metrics feature flag non-experimental ([PR #7033](https://github.com/apollographql/router/pull/7033))

Because the OTLP error metrics feature is being promoted to `preview` from `experimental`, this change updates its feature flag name from `experimental_otlp_error_metrics` to `preview_extended_error_metrics`.

By [@merylc](https://github.com/merylc) in https://github.com/apollographql/router/pull/7033



> [!TIP]
> All notable changes to Router v2.x after its initial release will be documented in this file.  To see previous history, see the [changelog prior to v2.0.0](https://github.com/apollographql/router/blob/1.x/CHANGELOG.md).

# [2.0.0] - 2025-02-17

This is a major release of the router containing significant new functionality and improvements to behaviour, resulting in more predictable resource utilisation and decreased latency.

Router 2.0.0 introduces general availability of Apollo Connectors, helping integrate REST services in router deployments.

This entry summarizes the overall changes in 2.0.0. To learn more details, go to the [What's New in router v2.x](https://www.apollographql.com/docs/graphos/routing/about-v2) page.

To upgrade to this version, follow the [upgrading from router 1.x to 2.x](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1) guide.

## ❗ BREAKING CHANGES ❗

In order to make structural improvements in the router and upgrade some of our key dependencies, some breaking changes were introduced in this major release. Most of the breaking changes are in the areas of configuration and observability. All details on what's been removed and changed can be found in the [upgrade guide](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1).

## 🚀 Features

Router 2.0.0 comes with many new features and improvements. While all the details can be found in the [What's New guide](https://www.apollographql.com/docs/graphos/routing/about-v2), the following features are the ones we are most excited about.

**Simplified integration of REST services using Apollo Connectors.** Apollo Connectors are a declarative programming model for GraphQL, allowing you to plug your existing REST services directly into your graph. Once integrated, client developers gain all the benefits of GraphQL, and API owners gain all the benefits of GraphOS, including incorporation into a supergraph for a comprehensive, unified view of your organization's data and services. [This detailed guide](https://www.apollographql.com/docs/graphos/schema-design/connectors/router) outlines how to configure connectors with the router.  Moving from Connectors Preview can be accomplished by following the steps in the [Connectors GA upgrade guide](https://www.apollographql.com/docs/graphos/schema-design/connectors/changelog).

**Predictable resource utilization and availability with back pressure.** Back pressure was not maintained in router 1.x, which meant _all_ requests were being accepted by the router. This resulted in issues for routers which are accepting high levels of traffic. Router 2.0.0 improves the handling of back pressure so that traffic shaping measures are more effective while also improving integration with telemetry. Improvements to back pressure then allows for significant improvements in traffic shaping, which improves router's ability to observe timeout and traffic shaping restrictions correctly. You can read about traffic shaping changes in [this section of the upgrade guide](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1#traffic-shaping).

**Metrics now all follow OpenTelemetry naming conventions.** Some of router's earlier metrics were created before the introduction of OpenTelemetry, resulting in naming inconsistencies. Along with standardising metrics to OpenTelemetry, traces submitted to GraphOS also default to using OpenTelemetry in router 2.0.0. Quite a few existing metrics had to be changed in order to do this properly and correctly, and we encourage you to carefully read through the upgrade guide for all the metrics changes.

**Improved validation of CORS configurations, preventing silent failures.** While CORS configuration did not change in router 2.0.0, we did improve CORS value validation. This results in things like invalid regex or unknown `allow_methods` returning errors early and preventing starting the router.

**Documentation for context keys, improving usability for advanced customers.** Router 2.0.0 creates consistent naming semantics for request context keys, which are used to share data across internal router pipeline stages. If you are relying on context entries in rust plugins, rhai scripts, coprocessors, or telemetry selectors, please refer to [this section](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1#context-keys) to see what keys changed.

## 📃 Configuration

Some changes to router configuration options were necessary in this release. Descriptions for both breaking changes to previous configuration and configuration for new features can be found in the [upgrade guide](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1)).

## 🛠 Maintenance

Many external Rust dependencies (crates) have been updated to modern versions where possible. As the Rust ecosystem evolves, so does the router. Keeping these crates up to date helps keep the router secure and stable.

Major upgrades in this version include:

- `axum`
- `http`
- `hyper`
- `opentelemetry`
- `redis`
