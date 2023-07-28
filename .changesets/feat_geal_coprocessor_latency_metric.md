### add a metric tracking coprocessor latency ([Issue #2924](https://github.com/apollographql/router/issues/2924))

Introduces a new metric for the router:

```
apollo.router.operations.coprocessor_request_time
```

It has one attributes:

```
coprocessor.stage: string (RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse)
```

It is an histogram metric tracking the time spent calling into the coprocessor

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3513