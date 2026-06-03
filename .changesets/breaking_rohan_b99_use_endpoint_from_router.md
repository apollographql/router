### Remove EndpointHandler and Endpoint::from_router_service ([PR #9215](https://github.com/apollographql/router/pull/9215))

Plugin authors who implement `web_endpoints()` must migrate from `Endpoint::from_router_service` to `Endpoint::from_router`.

**Before:**
```rust
use apollo_router::Endpoint;
use tower::Service;

fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
    let endpoint = Endpoint::from_router_service(
        "/my-path".to_string(),
        my_tower_service.boxed_clone(),
    );
    // ...
}
```

**After:**
```rust
use apollo_router::{axum, Endpoint};
use axum::routing::any;

fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
    let router = axum::Router::new().route("/", any(my_axum_handler));
    let endpoint = Endpoint::from_router("/my-path".to_string(), router);
    // ...
}
```

The `axum` crate is now re-exported as `apollo_router::axum`, so you no longer need to add it as a direct dependency. All axum router and handler types are available via this re-export.

The `plugin::Handler` struct has also been removed. It was an internal type used by `from_router_service` to wrap `BoxCloneService` in an `UnconstrainedBuffer` for thread-safe use from axum handlers. With the move to native axum Routers, `Handler` is no longer needed.

**Additional fixes included in this change:**

- Entity cache invalidation endpoint (`entity_cache.invalidation`) now correctly returns `Content-Type: application/json` on all responses (previously returned `text/plain`).
- Response cache invalidation endpoint (`response_cache.invalidation`) no longer accepts empty-body `POST []` requests as authenticated; an empty invalidation list is now rejected with `401 Unauthorized`.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9215
