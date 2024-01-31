### Fix header propagation issues ([Issue #4312](https://github.com/apollographql/router/issues/4312)), ([Issue #4398](https://github.com/apollographql/router/issues/4398))

This fixes two header propagation issues:
* if a client request header has already been added to a subgraph request due to another header propagation rule, then it is only added once
* `Accept`, `Accept-Encoding` and `Content-Encoding` were not in the list of reserved headers that cannot be propagated. They are now in that list because those headers are set explicitely by the Router in its subgraph requests

There is a potential regression: if a router deployment was accidentally relying on header propagation to compress subgraph requests, then it will not work anymore because `Content-Encoding` is not propagated anymore. Instead it should be set up from the `traffic_shaping` section of the Router configuration:

```yaml
traffic_shaping:
  all:
    compression: gzip
  subgraphs: # Rules applied to requests from the router to individual subgraphs
    products:
      compression: identity
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4535