### Remove synchronous `checkpoint` layer in favor of `checkpoint_async` ([ROUTER-1932](https://apollographql.atlassian.net/browse/ROUTER-1932))

The synchronous `ServiceBuilderExt::checkpoint()` method and its `CheckpointLayer` / `CheckpointService` implementation have been removed. Use `checkpoint_async()` instead.

`checkpoint_async` supports the same `ControlFlow::Continue` / `ControlFlow::Break` semantics, but the callback must be `async`.

**Migration example** — validating a request in `supergraph_service`:

Before:

```rust
ServiceBuilder::new()
    .checkpoint(|req: supergraph::Request| {
        if req.supergraph_request.body().operation_name.is_none() {
            Ok(ControlFlow::Break(
                supergraph::Response::error_builder()
                    .error(/* ... */)
                    .context(req.context)
                    .build()?,
            ))
        } else {
            Ok(ControlFlow::Continue(req))
        }
    })
    .service(service)
    .boxed_clone()
```

After:

```rust
ServiceBuilder::new()
    .checkpoint_async(|req: supergraph::Request| async move {
        if req.supergraph_request.body().operation_name.is_none() {
            Ok(ControlFlow::Break(
                supergraph::Response::error_builder()
                    .error(/* ... */)
                    .context(req.context)
                    .build()?,
            ))
        } else {
            Ok(ControlFlow::Continue(req))
        }
    })
    .service(service)
    .boxed_clone()
```

If you use `CheckpointLayer` or `CheckpointService` directly, switch to `AsyncCheckpointLayer` / `AsyncCheckpointService` from `apollo_router::layers::async_checkpoint`.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9706
