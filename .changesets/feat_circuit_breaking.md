### Add native circuit breaking for subgraphs and connectors ([PR #10300](https://github.com/apollographql/router/pull/10300))

The router can now stop sending requests to a subgraph or connector source that is already failing, and start sending them again once it recovers. Configure it with the new `circuit_breaker` section:

```yaml title="router.yaml"
circuit_breaker:
  all: # applied to every subgraph
    failure_rate_threshold: 0.5 # open once half of the window fails
    window_size: 100 # measure the rate over the last 100 requests
    min_requests: 10 # ...but only once 10 have been seen
    open_duration: 30s # stay open this long, then try one probe request
    consecutive_failures: 5 # or open immediately after 5 failures in a row
  subgraphs: # applied to individual subgraphs, in place of `all`
    products:
      consecutive_failures: 2
    reviews:
      window_size: 20
  connector:
    all: # applied to every connector source
      open_duration: 10s
    sources: # applied to individual sources, keyed by `<subgraph name>.<source name>`
      products.api:
        window_size: 500
```

Every option has a default. `circuit_breaker: {}` protects every subgraph and connector source using the defaults in the [Options table](https://www.apollographql.com/docs/graphos/routing/performance/circuit-breaking#options). Circuit breaking is off unless the `circuit_breaker` section is present, and protects every target once it is.

A subgraph or source listed under `subgraphs` or `connector.sources` takes its options from its own block alone: that block stands in for `all` rather than layering over it, so options it leaves out fall back to the defaults above and not to the values `all` gave them. `products` in the example above therefore opens after 2 failures in a row, over a window of 100 requests — `all`'s own `window_size`, had it set one, would not apply.

A circuit opens when either the failure rate over the last `window_size` requests reaches `failure_rate_threshold` (evaluated only once `min_requests` have been seen) or `consecutive_failures` requests fail in a row. The circuit sits between admission and execution. Traffic shaping's rate limit and load shedding, and a connector's `max_requests` limit, run before it, so a request they turn away isn't recorded at all. Everything that fulfills an admitted request runs behind it: coprocessors, rhai scripts, the response cache, native plugins, and the call itself, within the target's traffic shaping timeout. A `5xx` or `429` response, an error (including a connection that drops before the whole response arrives, or an unreachable coprocessor), and a request that runs past its timeout all count as failures. Any other `4xx` counts as a success. Nothing is recorded for a request the client abandons, or for a response that says nothing about the target's health: a coprocessor or rhai script turning the request away (such as a coprocessor `break` with a `401`), the router answering the request itself (such as a response cache hit, or demand control rejecting a request as too expensive), or a file upload whose stream from the client fails part way through. A probe that ends this way hands the probe to the next request. Mapping-only connectors never make a request, so they are invisible to the circuit and keep being served while it is open. Subgraph fetches that join a subgraph batch go around the circuit too, so an open circuit can't hold up the rest of the batch. A fetch to a subgraph with batching disabled joins no batch, so it still meets the circuit.

Invalid options, such as a `min_requests` larger than its `window_size`, stop the router from starting and point at the offending key.

Circuits are rebuilt with the rest of the request pipeline, so every circuit starts closed again when the router reloads its schema or configuration.

While a circuit is open, the affected fetch fails immediately with a `REQUEST_CIRCUIT_BREAKER_OPEN` error instead of waiting on a call that is unlikely to succeed, before coprocessors, rhai scripts, native plugins, or the response cache run for it. After `open_duration` a single probe request is let through: if it succeeds the circuit closes, and if it fails the circuit stays open for another `open_duration`. If the probe's client goes away first, the next request becomes the probe.

Each circuit reports which state it is in through `apollo.qos.circuit_breaker.state` (`1` for the state named by the metric's own `apollo.qos.circuit_breaker.state` attribute and `0` for the other two), and its traffic through the `apollo.qos.circuit_breaker.requests` and `apollo.qos.circuit_breaker.transitions` counters, all attributed by circuit name.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10300
