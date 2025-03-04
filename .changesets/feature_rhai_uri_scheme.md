### Support to get/set URI scheme in Rhai ([Issue #6897](https://github.com/apollographql/router/issues/6897))

This adds support to read the scheme from the `request.uri.scheme`/`request.subgraph.uri.scheme` functions in Rhai, 
enabling the ability to switch between http and https for subgraph fetches. For example:

```rs
fn subgraph_service(service, subgraph){
    service.map_request(|request|{
        log_info(`${request.subgraph.uri.scheme}`);
        if request.subgraph.uri.scheme == {} {
            log_info("Scheme is not explicitly set");
        }
        request.subgraph.uri.scheme = "https"
        request.subgraph.uri.host = "api.apollographql.com";
        request.subgraph.uri.path = "/api/graphql";
        request.subgraph.uri.port = 1234;
        log_info(`${request.subgraph.uri}`);
    });
}
```
By [@starJammer](https://github.com/starJammer) in https://github.com/apollographql/router/pull/5439