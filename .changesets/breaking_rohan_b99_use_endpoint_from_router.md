### Remove EndpointHandler and Endpoint::from_router_service ([PR #9215](https://github.com/apollographql/router/pull/9215))

`Endpoint::from_router_service` and `plugin::Handler` are removed. Use `Endpoint::new` with an `EndpointService` built from your own HTTP `BoxCloneService` (or any cloneable Tower service over `http::Request` / `http::Response` with `Error = Infallible`).

**Before:**
```rust
Endpoint::from_router_service("/my-path".to_string(), my_router_box_service)
```

**After:**
```rust
Endpoint::new(
    "/my-path".to_string(),
    EndpointService::new(my_http_box_clone_service),
)
```

**Additional fixes:**

- Entity cache invalidation (`entity_cache.invalidation`) returns `Content-Type: application/json` on all responses.
- Response cache invalidation (`response_cache.invalidation`) rejects empty `POST []` bodies with `401 Unauthorized`.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9215
