### Support reading and setting `port` on request URIs using Rhai ([Issue #5437](https://github.com/apollographql/router/issues/5437))

Custom Rhai scripts in the router now support the `request.uri.port` and `request.subgraph.uri.port` functions for reading and setting URI ports. These functions enable you to update the full URI for subgraph fetches. For example: 

```rust
fn subgraph_service(service, subgraph){
    service.map_request(|request|{
        log_info(`${request.subgraph.uri.port}`);
        if request.subgraph.uri.port == {} {
            log_info("Port is not explicitly set");
        }
        request.subgraph.uri.host = "api.apollographql.com";
        request.subgraph.uri.path = "/api/graphql";
        request.subgraph.uri.port = 1234;
        log_info(`${request.subgraph.uri}`);
    });
}
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5439
