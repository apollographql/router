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
## 🚀 Features

### Add support for dhat based heap profiling ([PR #1829](https://github.com/apollographql/router/pull/1829))

[dhat-rs](https://github.com/nnethercote/dhat-rs) provides [DHAT](https://www.valgrind.org/docs/manual/dh-manual.html) style heap profiling. We have added two compile features, dhat-heap and dhat-ad-hoc, which leverage this ability.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1829

### Add `trace_id` in logs to identify all logs related to a specific request ([Issue #1981](https://github.com/apollographql/router/issues/1981))

It automatically adds a `trace_id` on logs to identify which log is related to a specific request. Also adds `apollo_trace_id` in response headers to help the client to identify logs for this request.

Example of logs in text:

```logs
2022-10-21T15:17:45.562553Z ERROR [trace_id=5e6a6bda8d0dca26e5aec14dafa6d96f] apollo_router::services::subgraph_service: fetch_error="hyper::Error(Connect, ConnectError(\"tcp connect error\", Os { code: 111, kind: ConnectionRefused, message: \"Connection refused\" }))"
2022-10-21T15:17:45.565768Z ERROR [trace_id=5e6a6bda8d0dca26e5aec14dafa6d96f] apollo_router::query_planner::execution: Fetch error: HTTP fetch failed from 'accounts': HTTP fetch failed from 'accounts': error trying to connect: tcp connect error: Connection refused (os error 111)
```

Example of logs in JSON:

```logs
{"timestamp":"2022-10-26T15:39:01.078260Z","level":"ERROR","fetch_error":"hyper::Error(Connect, ConnectError(\"tcp connect error\", Os { code: 111, kind: ConnectionRefused, message: \"Connection refused\" }))","target":"apollo_router::services::subgraph_service","filename":"apollo-router/src/services/subgraph_service.rs","line_number":182,"span":{"name":"subgraph"},"spans":[{"trace_id":"5e6a6bda8d0dca26e5aec14dafa6d96f","name":"request"},{"name":"supergraph"},{"name":"execution"},{"name":"parallel"},{"name":"fetch"},{"name":"subgraph"}]}
{"timestamp":"2022-10-26T15:39:01.080259Z","level":"ERROR","message":"Fetch error: HTTP fetch failed from 'accounts': HTTP fetch failed from 'accounts': error trying to connect: tcp connect error: Connection refused (os error 111)","target":"apollo_router::query_planner::execution","filename":"apollo-router/src/query_planner/execution.rs","line_number":188,"span":{"name":"parallel"},"spans":[{"trace_id":"5e6a6bda8d0dca26e5aec14dafa6d96f","name":"request"},{"name":"supergraph"},{"name":"execution"},{"name":"parallel"}]}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1982

### Reload the configuration when receiving the SIGHUP signal ([Issue #35](https://github.com/apollographql/router/issues/35))

This adds support for reloading configuration when receiving the SIGHUP signal. This only works on unix-like platforms,
and only with the configuration file.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2015

## 🐛 Fixes

### Fix the deduplication logic in deduplication caching ([Issue #1984](https://github.com/apollographql/router/issues/1984))

Under load, it is possible to break the router deduplication logic and leave orphaned entries in the waiter map. This fixes the logic to prevent this from occurring.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2014

### Follow directives from Uplink ([Issue #1494](https://github.com/apollographql/router/issues/1494) [Issue #1539](https://github.com/apollographql/router/issues/1539))

The Uplink API returns actionable info in its responses:
- some error codes indicate an unrecoverable issue, for which the router should not retry the query (example: non-existing graph)
- it can tell the router when it should retry the query

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2001

### Fix the rhai SDL print function ([Issue #2005](https://github.com/apollographql/router/issues/2005))

A recent change to the way we provide the SDL to plugins broke the rhai SDL print. This fixes it.

By [@fernando-apollo](https://github.com/fernando-apollo) in https://github.com/apollographql/router/pull/2007

### Exports a missing strut (`router_factory::Endpoint`) that was preventing the `web_endpoints` trait from being implemented by Plugins

By [@scottdouglas1989](https://github.com/scottdouglas1989) in https://github.com/apollographql/router/pull/2007

### Validate default values for input object fields ([Issue #1979](https://github.com/apollographql/router/issues/1979))

When validating variables, we should use default values for object fields if applicable.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2003

### Replace the BatchSpanProcessor with SimpleSpanProcessor ([Issue #2046](https://github.com/apollographql/router/issues/2046))

The way telemetry processes span had scalability issues, due to batching spans before sending them.
The processor is replaced with one that sends spans to the exporter as soon as they are created.
The exporter is then tasked with aggregating or batching them as needed, according to its
protocol.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2047


## 🛠 Maintenance

### Apply tower best practice to inner service cloning ([PR #2030](https://github.com/apollographql/router/pull/2030))

Our service readiness checking can be improved by following tower project recommendations for cloning inner services.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2030

### Split the configuration file management in multiple modules ([Issue #1790](https://github.com/apollographql/router/issues/1790))

The file is becoming large and hard to modify.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1996

## 📚 Documentation

### Remove references to git submodules from DEVELOPMENT.md ([Issue #2012](https://github.com/apollographql/router/issues/2012))

We don't need instructions about submodules since #1856. Let's remove them.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2045

