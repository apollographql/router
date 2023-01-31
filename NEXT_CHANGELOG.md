# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <KEEP> THIS IS AN SET OF TEMPLATES TO USE WHEN ADDING TO THE CHANGELOG.

## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 📃 Configuration
Configuration changes will be [automatically migrated on load](https://www.apollographql.com/docs/router/configuration/overview#upgrading-your-router-configuration). However, you should update your source configuration files as these will become breaking changes in a future major release.
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
</KEEP> -->

## 🚀 Features

### Always deduplicate variables ([Issue #2387](https://github.com/apollographql/router/issues/2387))

Variable deduplication allows the router to reduce the number of entities that are requested from subgraphs if some of them are redundant, and as such reduce the size of subgraph responses. It has been available for a while but was not active by default. This is now always on.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2445

### Add optional `Access-Control-Max-Age` header to CORS plugin ([Issue #2212](https://github.com/apollographql/router/issues/2212))

Adds new option called `max_age` to the existing `cors` object which will set the value returned in the [`Access-Control-Max-Age`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Access-Control-Max-Age) header. As was the case previously, when this value is not set **no** value is returned.

It can be enabled using our standard time notation, as follows:

```
cors:
  max_age: 1day
```

By [@osamra-rbi](https://github.com/osamra-rbi) in https://github.com/apollographql/router/pull/2331

### Improved support for wildcards in `supergraph.path` configuration ([Issue #2406](https://github.com/apollographql/router/issues/2406))

You can now use a wildcard in supergraph endpoint `path` like this:

```yaml
supergraph:
  listen: 0.0.0.0:4000
  path: /graph*
```

In this example, the Router would respond to requests on both `/graphql` and `/graphiql`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2410


## 🐛 Fixes

### Forbid caching `PERSISTED_QUERY_NOT_FOUND` responses ([Issue #2502](https://github.com/apollographql/router/issues/2502))

The router now sends a `cache-control: private, no-cache, must-revalidate` response header to clients, in addition to the existing `PERSISTED_QUERY_NOT_FOUND` error code on the response which was being sent previously.  This expanded behaviour occurs when when a persisted query hash could not be found and is important since such responses should **not** be cached by intermediary proxies/CDNs since the client will need to be able to send the full query directly to the Router on a subsequent request.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2503
### Listen on root URL when `/*` is set in `supergraph.path` configuration ([Issue #2471](https://github.com/apollographql/router/issues/2471))

This resolves a regression which occurred in Router 1.8 when using wildcard notation on a path-boundary, as such:

```yaml
supergraph:
  path: /*
```

This occurred due to an underlying [Axum upgrade](https://github.com/tokio-rs/axum/releases/tag/axum-v0.6.0) and resulted in failure to listen on `localhost` when a path was absent. We now special case `/*` to also listen to the URL without a path so you're able to call `http://localhost` (for example).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2472

### Return a proper timeout response ([Issue #2360](https://github.com/apollographql/router/issues/2360) [Issue #2400](https://github.com/apollographql/router/issues/240))

There was a regression where timeouts resulted in a HTTP response of `500 Internal Server Error`. This is now fixed with a test to guarantee it, the status code is now `504 Gateway Timeout` (instead of the previous `408 Request Timeout` which, was also incorrect in that it blamed the client).

There is also a new metric emitted called `apollo_router_timeout` to track when timeouts are triggered.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2419

### Fix panic in schema parse error reporting ([Issue #2269](https://github.com/apollographql/router/issues/2269))

In order to support introspection, some definitions like `type __Field { … }` are implicitly added to schemas. This addition was done by string concatenation at the source level. In some cases like unclosed braces, a parse error could be reported at a position beyond the size of the original source. This would cause a panic because only the unconcatenated string is sent to the error reporting library `miette`.

Instead, the Router now parses introspection types separately and "concatenates" the definitions at the AST level.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2448

### Always accept compressed subgraph responses  ([Issue #2415](https://github.com/apollographql/router/issues/2415))

Previously, subgraph response decompression was only supported when subgraph request compression was _explicitly_ configured. This is now always active.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2450

### Fix handling of root query operations not named `Query`

If you'd mapped your default `Query` type to something other than the default using `schema { query: OtherQuery }`, some parsing code in the Router would incorrectly return an error because it had previously assumed the default name of `Query`. The same case would have occurred if the root mutation type was not named `Mutation`.

This is now corrected and the Router understands the mapping.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2459

### Remove the `locations` field from subgraph errors ([Issue #2297](https://github.com/apollographql/router/issues/2297))

Subgraph errors can come with a `locations` field indicating which part of the query was causing issues, but it refers to the subgraph query generated by the query planner, and we have no way of translating it to locations in the client query. To avoid confusion, we've removed this field from the response until we can provide a more coherent way to map these errors back to the original operation.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2442

### Emit metrics showing number of client connections ([issue #2384](https://github.com/apollographql/router/issues/2384))

New metrics are available to track the client connections:

- `apollo_router_session_count_total` indicates the number of currently connected clients
- `apollo_router_session_count_active` indicates the number of in flight GraphQL requests from connected clients.

This also fixes the behaviour when we reach the maximum number of file descriptors: instead of going into a busy loop, the router will wait a bit before accepting a new connection.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2395

### `--dev` will no longer modify configuration that it does not directly touch ([Issue #2404](https://github.com/apollographql/router/issues/2404), [Issue #2481](https://github.com/apollographql/router/issues/2481))

Previously, the Router's `--dev` mode was operating against the configuration object model. This meant that it would sometimes replace pieces of configuration where it should have merely modified it.  Now, `--dev` mode will _override_ the following properties in the YAML config, but it will leave any adjacent configuration as it was:

```yaml
homepage:
  enabled: false
include_subgraph_errors:
  all: true
plugins:
  experimental.expose_query_plan: true
sandbox:
  enabled: true
supergraph:
  introspection: true
telemetry:
  tracing:
    experimental_response_trace_id:
      enabled: true
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2489

## 🛠 Maintenance

### Improve #[serde(default)] attribute on structs ([Issue #2424](https://github.com/apollographql/router/issues/2424))

If all the fields of your `struct` have their default value then use the `#[serde(default)]` on the `struct` instead of on each field. If you have specific default values for a field, you'll have to create your own `impl Default` for the `struct`.

#### Correct approach

```rust
#[serde(deny_unknown_fields, default)]
struct Export {
    url: Url,
    enabled: bool
}

impl Default for Export {
  fn default() -> Self {
    Self {
      url: default_url_fn(),
      enabled: false
    }
  }
}
```

#### Discouraged approach

```rust
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(default="default_url_fn")
    url: Url,
    #[serde(default)]
    enabled: bool
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2424

## 📃 Configuration

Configuration changes will be [automatically migrated on load](https://www.apollographql.com/docs/router/configuration/overview#upgrading-your-router-configuration). However, you should update your source configuration files as these will become breaking changes in a future major release.

### `health-check` has been renamed to `health_check` ([Issue #2161](https://github.com/apollographql/router/issues/2161))

The `health_check` option in the configuration has been renamed to use `snake_case` rather than `kebab-case` for consistency with the other properties in the configuration:

```diff
-health-check:
+health_check:
   enabled: true
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2451 and https://github.com/apollographql/router/pull/2463

## 📚 Documentation

### Disabling anonymous usage metrics ([Issue #2478](https://github.com/apollographql/router/issues/2478))

To disable the anonymous usage metrics, you set `APOLLO_TELEMETRY_DISABLED=true` in the environment.  The documentation previously said to use `1` as the value instead of `true`.  In the future, either will work, so this is primarily a bandaid for the immediate error.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2479

### `send_headers` and `send_variable_values` in `telemetry.apollo` ([Issue #2149](https://github.com/apollographql/router/issues/2149))

+ `send_headers`

  Provide this field to configure which request header names and values are included in trace data that's sent to Apollo Studio. Valid options are: `only` with an array, `except` with an array, `none`, `all`.

  The default value is `none``, which means no header names or values are sent to Studio. This is a security measure to prevent sensitive data from potentially reaching the Router.

+ `send_variable_values`

  Provide this field to configure which variable values are included in trace data that's sent to Apollo Studio. Valid options are: `only` with an array, `except` with an array, `none`, `all`.

  The default value is `none`, which means no variable values are sent to Studio. This is a security measure to prevent sensitive data from potentially reaching the Router.


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2435

### Propagating headers between subgraphs ([Issue #2128](https://github.com/apollographql/router/issues/2128))

Passing headers between subgraph services is possible via Rhai script. An example has been added to the header propagation page.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2446

### IPv6 listening instructions ([Issue #1835](https://github.com/apollographql/router/issues/1835))

Added documentation for listening on IPv6
```yaml
supergraph:
  # The socket address and port to listen on.
  # Note that this must be quoted to avoid interpretation as a yaml array.
  listen: '[::1]:4000'
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2440

## 🛠 Maintenance

### Parse schemas and queries with `apollo-compiler`

The Router now uses the higher-level representation (HIR) from `apollo-compiler` instead of using the AST from `apollo-parser` directly.  This is a first step towards replacing a bunch of code that grew organically during the Router's early days, with a general-purpose library with intentional design.  Internal data structures are unchanged for now.  Parsing behavior has been tested to be identical on a large corpus of schemas and queries.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2466

### Disregard value of `APOLLO_TELEMETRY_DISABLED` in Orbiter unit tests ([Issue #2487](https://github.com/apollographql/router/issues/2487))

The `orbiter::test::test_visit_args` tests were failing in the event that `APOLLO_TELEMETRY_DISABLED` was set, however this is now corrected.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2488

## 🥼 Experimental

### JWT authentication ([Issue #912](https://github.com/apollographql/router/issues/912))

As a result of UX feedback, we are modifying the experimental JWT configuration. The `jwks_url` parameter is renamed to `jwks_urls` and now expects to receive an array of URLs, rather than a single URL.

Here's a typical sample configuration fragment:

```yaml
authentication:
  experimental:
    jwt:
      jwks_urls:
        - https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2500

