### fix(response_cache): change plugin ordering to make sure we can customize caching behavior at the subgraph level ([PR #8652](https://github.com/apollographql/router/pull/8652))

With this change you'll now be able to customize cached response using rhai or coprocessors. It's also now possible to set a different [`private_id`](https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/customization#configure-private_id) based on subgraph request (like headers for example). 

Example of rhai script customizing the `private_id`:

```rhai
fn subgraph_service(service, subgraph) {
    service.map_request(|request| {
        if "private_id" in request.headers {
            request.context["private_id"] = request.headers["private_id"];
        }
    });
}

```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8652