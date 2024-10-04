### Add request.uri.port to rhai engine ([PR #6119](https://github.com/apollographql/router/pull/6119))

`request.uri.port` and `request.subgraph.uri.port` to rhai engine for both read and write. An example fo where this may be useful is if you need to dynamically change the uri of a subgraph call, including setting a port:

```rhai
fn subgraph_request_callback (request) {
  request.subgraph.uri.port = "4001";
}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6119
