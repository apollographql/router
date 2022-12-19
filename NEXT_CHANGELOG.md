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
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## 🚀 Features

### Apollo uplink: Configurable schema poll timeout ([PR #2271](https://github.com/apollographql/router/pull/2271))

In addition to the url and poll interval, Uplink poll timeout can now be configured via command line arg and env variable:

```bash
        --apollo-uplink-timeout <APOLLO_UPLINK_TIMEOUT>
            The timeout for each of the polls to Apollo Uplink. [env: APOLLO_UPLINK_TIMEOUT=] [default: 30s]
```

It defaults to 30 seconds.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2271

## 🐛 Fixes

### Return root `__typename` in first chunk of defer response when first response is empty ([Issue #1922](https://github.com/apollographql/router/issues/1922))

With this query:

```graphql
{
  __typename
  ...deferedFragment @defer
}

fragment deferedFragment on Query {
  slow
}
```

You will receive the first response chunk:

```json
{"data":{"__typename": "Query"},"hasNext":true}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2274

### Change log level when we can't get the schema from GCP ([Issue #2004](https://github.com/apollographql/router/issues/2004))

Set the log level for this specific log to `debug`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2215

### Traces won't cause missing field-stats ([Issue #2267](https://github.com/apollographql/router/issues/2267))

Previously if a request was sampled for tracing it was not contributing to metrics correctly. This was a particular problem for users with a high sampling rate.
Now metrics and traces have been separated so that metrics are always comprehensive and traces are ancillary.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2277

### Replace notify recommended watcher with PollWatcher ([Issue #2245](https://github.com/apollographql/router/issues/2245))

We noticed that we kept receiving issues about hot reload. We tried to fix this a while back by moving from HotWatch to Notify, but there are still issues. The problem appears to be caused by having different mechanisms on different platforms. Switching to the PollWatcher, which offers less sophisticated functionality but is the same on all platforms, should solve these issues at the expense of slightly worse reactiveness.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2276

### Keep the error path when redacting subgraph errors ([Issue #1818](https://github.com/apollographql/router/issues/1818))

Error redaction was erasing the error's path, which made it impossible to affect the errors to deferred responses. Now the redacted errors keep the path. Since the response shape for the primary and deferred responses are defined from the API schema, there is no possibility of leaking internal schema information here.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2273

### Wrong urldecoding for variables in get requests ([Issue #2248](https://github.com/apollographql/router/issues/2248))

Using APQs, any '+' characters would be replaced by spaces in variables, breaking for instance datetimes with timezone info.

By [@neominik](https://github.com/neominik) in https://github.com/apollographql/router/pull/2249

## 🛠 Maintenance

### Return more consistent errors ([Issue #2101](https://github.com/apollographql/router/issues/2101))

Change some of our errors we returned by following [this specs](https://www.apollographql.com/docs/apollo-server/data/errors/). It adds a `code` field in `extensions` describing the current error. 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2178

## 🥼 Experimental


### Introduce a `router_service` ([Issue #1496](https://github.com/apollographql/router/issues/1496))

A `router_service` is now part of our service stack, which allows plugin developers to process raw http requests and raw http responses, that wrap the already available `supergraph_service`

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2170
