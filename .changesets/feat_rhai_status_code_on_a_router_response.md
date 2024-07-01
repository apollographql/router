### Make `status_code` available for `router_service` responses in Rhai scripts ([Issue #5357](https://github.com/apollographql/router/issues/5357))

Adds `response.status_code` on Rhai [`router_service`](https://www.apollographql.com/docs/router/customizations/rhai-api/#entry-point-hooks) responses. Previously, `status_code` was only available on `subgraph_service` responses. 

For example:

```rust
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
