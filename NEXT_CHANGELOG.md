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

## ❗ BREAKING ❗
## 🚀 Features

### Scaffold: Add a Dockerfile and document it ([#2295](https://github.com/apollographql/router/issues/2295))

Projects you create via scaffold will now have a Dockerfile so you can build and ship a custom router container.
The docs have been updated with links and steps to build your custom router with plugins.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2307

### Apollo uplink: Configurable schema poll timeout ([PR #2271](https://github.com/apollographql/router/pull/2271))

In addition to the url and poll interval, Uplink poll timeout can now be configured via command line arg and env variable:

```bash
        --apollo-uplink-timeout <APOLLO_UPLINK_TIMEOUT>
            The timeout for each of the polls to Apollo Uplink. [env: APOLLO_UPLINK_TIMEOUT=] [default: 30s]
```

It defaults to 30 seconds.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2271

### Warm up the query plan cache on schema updates ([Issue #2302](https://github.com/apollographql/router/issues/2302), [Issue #2308](https://github.com/apollographql/router/issues/2308))

When the schema changes, queries have to go through the query planner again to get the plan cached, which creates latency
instabilities. There is now an option to select the most used queries from the query plan cache and run them again through
the query planner before switching the router to the new schema. This slows down the switch but the most used queries will
immediately use the cache.

This can be configured as follows:

```yaml
supergraph:
  query_planning:
    # runs the 100 most used queries through the query planner on schema changes
    warmed_up_queries: 100 # The default is 0, which means do not warm up.
    experimental_cache:
      in_memory:
        # sets the limit on the number of entries in the in memory query plan cache
        limit: 512
```

Query planning was also updated to finish executing and setting up the cache even if the client timeouts and cancels the request.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2309

## 🐛 Fixes

### Propagate errors across inline fragments

GraphQL errors now correctly propagate across inline fragments.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2304

### Only rebuild protos if proto source changes

By [@scottdouglas1989](https://github.com/scottdouglas1989) in https://github.com/apollographql/router/pull/2283

### Return an error on duplicate keys in configuration ([Issue #1428](https://github.com/apollographql/router/issues/1428))

If you have duplicated keys in your yaml configuration like this:

```yaml
telemetry:
  tracing:
    propagation:
      jaeger: true
  tracing:
    propagation:
      jaeger: false
```

It will now throw an error on router startup:

`ERROR duplicated keys detected in your yaml configuration: 'telemetry.tracing'`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2270

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

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2277 and https://github.com/apollographql/router/pull/2286

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

### Add more details when GraphQL request is invalid ([Issue #2301](https://github.com/apollographql/router/issues/2301))

Add more context to the error we're throwing if your GraphQL request is invalid, here is an exemple response if you pass `"variables": "null"` in your JSON payload.
```json
{
  "errors": [
    {
      "message": "Invalid GraphQL request",
      "extensions": {
        "details": "failed to deserialize the request body into JSON: invalid type: string \"null\", expected a map at line 1 column 100",
        "code": "INVALID_GRAPHQL_REQUEST"
      }
    }
  ]
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2306

### Add outgoing request URLs for the subgraph calls in the OTEL spans ([Issue #2280](https://github.com/apollographql/router/issues/2280))

Add attribute named `http.url` containing the subgraph URL in span `subgraph_request`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2291

### Return more consistent errors ([Issue #2101](https://github.com/apollographql/router/issues/2101))

Change some of our errors we returned by following [this specs](https://www.apollographql.com/docs/apollo-server/data/errors/). It adds a `code` field in `extensions` describing the current error.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2178

## 🥼 Experimental

### Introduce a `router_service` ([Issue #1496](https://github.com/apollographql/router/issues/1496))

A `router_service` is now part of our service stack, which allows plugin developers to process raw http requests and raw http responses, that wrap the already available `supergraph_service`

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2170

### Introduce an externalization mechanism based on `router_service` ([Issue #1916](https://github.com/apollographql/router/issues/1916))

If external extensibility is configured, then a block of data is transmitted (encoded as JSON) to an endpoint via an HTTP POST request. The router will process the response to the POST request before resuming execution.

Conceptually, an external co-processor performs the same functionality as you may provide via a rust plugin or a rhai script within the router. The difference is the protocol which governs the interaction between the router and the co-processor.

Sample configuration:

```yaml
plugins:
  experimental.external:
    url: http://127.0.0.1:8081 # mandatory URL which is the address of the co-processor
    timeout: 2s # optional timeout (2 seconds in this example). If not set, defaults to 1 second
    stages: # In future, multiple stages may be configurable
      router: # Currently, the only valid value is router
        request: # What data should we transmit to the co-processor from the router request?
          headers: true # All of these data content attributes are optional and false by default.
          context: true
          body: true
          sdl: true
        response: # What data should we transmit to the co-processor from the router response?
          headers: true
          context: true
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2229
