### Rhai: `status_code` is available at the `router_service` ([Issue #5357](https://github.com/apollographql/router/issues/5357))

Following up on the introduction of [`status_code`](https://www.apollographql.com/docs/router/customizations/rhai-api/#responsestatus_codeto_string)  as an available property in Rhai's `subgraph_service` response, we've now introduced the availability of the property on the `router_service` response as well:

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
