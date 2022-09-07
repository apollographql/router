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

# [x.x.x] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Unified supergraph and execution response types

`apollo_router::services::supergraph::Response` and 
`apollo_router::services::execution::Response` were two structs with identical fields
and almost-identical methods.
The main difference was that builders were fallible for the former but not the latter.

They are now the same type (with one location a `type` alias of the other), with fallible builders.
Callers may need to add either a operator `?` (in plugins) or an `.unwrap()` call (in tests).

```diff
 let response = execution::Response::builder()
     .error(error)
     .status_code(StatusCode::BAD_REQUEST)
     .context(req.context)
-    .build();
+    .build()?;
```

By [@SimonSapin](https://github.com/SimonSapin)

### Rename `originating_request` to `supergraph_request` on various plugin `Request` structures ([Issue #1713](https://github.com/apollographql/router/issues/1713))

We feel that `supergraph_request` makes it more clear that this is the request received from the client.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1715

### Allow users to customize the prometheus endpoint URL ([Issue #1645](https://github.com/apollographql/router/issues/1645))

The prometheus endpoint now listens to 0.0.0.0:9090/metrics by default. It previously listened to http://0.0.0.0:4000/plugins/apollo.telemetry/prometheus
The Router's Prometheus interface is now exposed at `127.0.0.1:9090/metrics`, rather than `http://0.0.0.0:4000/plugins/apollo.telemetry/prometheus`.  This should be both more secure and also more generally compatible with the default settings that Prometheus expects (which also uses port `9090` and just `/metrics` as its defaults).

To expose to a non-localhost interface, it is necessary to explicitly opt-into binding to a socket address of `0.0.0.0:9090` (i.e., all interfaces on port 9090) or a specific available interface (e.g., `192.168.4.1`) on the host.

Have a look at the Features section to learn how to customize the listen address and the path

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1654

## 🚀 Features

### New plugin helper: `map_first_graphql_response`

In supergraph and execution services, the service response contains
not just one GraphQL response but a stream of them,
in order to support features such as `@defer`.

This new method of `ServiceExt` and `ServiceBuilderExt` in `apollo_router::layers`
wraps a service and calls a `callback` when the first GraphQL response
in the stream returned by the inner service becomes available.
The callback can then access the HTTP parts (headers, status code, etc)
or the first GraphQL response before returning them.

See the doc-comments in `apollo-router/src/layers/mod.rs` for more.

By [@SimonSapin](https://github.com/SimonSapin)

### Allow users to customize the prometheus endpoint URL ([Issue #1645](https://github.com/apollographql/router/issues/1645))

You can now customize the prometheus endpoint URL in your yml configuration:

```yml
telemetry:
  metrics:
    prometheus:
      listen: 127.0.0.1:9090 # default
      path: /metrics # default
      enabled: true
```

By [@o0Ignition0o](https://github.com/@o0Ignition0o) in https://github.com/apollographql/router/pull/1654

## 🐛 Fixes

### Fix metrics duration for router request ([#1705](https://github.com/apollographql/router/issues/1705))

With the introduction of `BoxStream` for defer we introduced a bug when computing http request duration metrics. Sometimes we didn't wait for the first response in the `BoxStream`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1705

### Numerous fixes to preview `@defer` query planning ([Issue #1698](https://github.com/apollographql/router/issues/1698))

Updated to [Federation `2.1.2-alpha.0`](https://github.com/apollographql/federation/pull/2132) which brings in a number of fixes for the preview `@defer` support.  These fixes include:

 - [Empty selection set produced with @defer'd query `federation#2123`](https://github.com/apollographql/federation/issues/2123)
 - [Include directive with operation argument errors out in Fed 2.1 `federation#2124`](https://github.com/apollographql/federation/issues/2124)
 - [query plan sequencing affected with __typename in fragment `federation#2128`](https://github.com/apollographql/federation/issues/2128)
 - [Router Returns Error if __typename Omitted `router#1668`](https://github.com/apollographql/router/issues/1668)

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1711

## 🛠 Maintenance
## 📚 Documentation
