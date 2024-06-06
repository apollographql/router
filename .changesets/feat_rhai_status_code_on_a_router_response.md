### Add support for router response `status_code` to rhai ([Issue #5357](https://github.com/apollographql/router/issues/5357))

Previous `status_code` was accessible only on subgraph response, now you can use it on router responses:
```
fn router_service(service) {
    let f = |response| {
        if response.is_primary() {
            print(response.status_code);
        }
    };

    service.map_response(f);
}
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5358
