### Customize response caching behavior at the subgraph level ([PR #8652](https://github.com/apollographql/router/pull/8652))

You can now customize cached responses using Rhai or coprocessors. You can also set a different [`private_id`](https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/customization#configure-private_id) based on subgraph request headers.

**Example Rhai script customizing `private_id`:**

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