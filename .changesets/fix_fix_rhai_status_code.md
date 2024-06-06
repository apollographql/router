### Add missing `status_code` on router, supergraph and execution responses exposed in Rhai scripts ([Issue #5357](https://github.com/apollographql/router/issues/5357))

Previous `status_code` was accessible only on subgraph response, after this fix it will work on all responses.

For example:
```
fn supergraph_service(service) {
    let f = |response| {
        if response.is_primary() {
            print(response.status_code);
        }
    };

    service.map_response(f);
}
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5358
