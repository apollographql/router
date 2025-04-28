# Changelog

This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).

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
