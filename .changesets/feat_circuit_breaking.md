### Add native circuit breaking for subgraphs and connectors ([Issue #ROUTER-2056](https://github.com/apollographql/router/issues/ROUTER-2056))

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
      enabled: false # opt this subgraph out of `all`
  connector:
    all: # applied to every connector source
      open_duration: 10s
    sources: # applied to individual sources, keyed by `<subgraph name>.<source name>`
      products.api:
        window_size: 500
```

Every option is optional and defaults to the value shown above, so `circuit_breaker: {all: {}}` is enough to protect every subgraph with sensible thresholds. Circuit breaking is off unless configured.

A subgraph or source listed under `subgraphs` or `connector.sources` takes its options from its own block alone: that block stands in for `all` rather than layering over it, so options it leaves out fall back to the defaults above and not to the values `all` gave them. `products` in the example above therefore opens after 2 failures in a row, over a window of 100 requests — `all`'s own `window_size`, had it set one, would not apply.

`enabled: false` leaves a target out of circuit breaking entirely, and leaves any options in the same block in place for when you switch it back on.

A circuit opens when either the failure rate over the last `window_size` requests reaches `failure_rate_threshold` — evaluated only once `min_requests` have been seen — or `consecutive_failures` requests fail in a row. A 5xx response, a transport error, and a request cancelled before the service answered all count as failures. A request the router turned away itself does not: a connector `max_requests` limit, a rate limit, or a coprocessor breaking a request says nothing about the target's health. Responses the router answers without asking the target, response cache hits above all, are equally invisible to the circuit and keep being served while it is open.

While a circuit is open, the affected fetch fails immediately with a `REQUEST_CIRCUIT_BREAKER_OPEN` error instead of waiting on a call that is unlikely to succeed. After `open_duration` a single probe request is let through: if it succeeds the circuit closes, and if it fails the circuit stays open for another `open_duration`.

Each circuit reports its state through the `apollo.qos.circuit_breaker.state` gauge, and its traffic through the `apollo.qos.circuit_breaker.requests` and `apollo.qos.circuit_breaker.state.transitions` counters, all attributed by circuit name.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9974
