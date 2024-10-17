### Support to get/set URI port in Rhai ([Issue #5437](https://github.com/apollographql/router/issues/5437))

This adds support to read the port from the `request.uri.port`/`request.subgraph.uri.port` functions in Rhai, enabling the ability to update the full URI for subgraph fetches. For example: 

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
