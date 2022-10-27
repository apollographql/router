# Async authentication

Demonstrates use of `checkpoint_async` to perform authentication that depends on an asynchronous call.

## Usage

```bash
cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml
```

## Implementation

In this example a file is read to check for an ID, but it could be any async call, for example to an external
authentication server.

`checkpoint` and `checkpoint_async` allow you to halt request and return immediately. This is particularly useful for authentication.

```rust
    fn supergraph_service(
        &mut self,
        service: router::BoxService,
    ) -> router::BoxService {
        ServiceBuilder::new()
            .checkpoint_async(...) // Authentication happens here
            .buffer(20_000) // Required, see note below
            .service(service)
            .boxed()
    }
```

Note that layers that require a service to be moved across an `await` point, e.g. `checkpoint_async` or `filter_async`
must be followed by a call to buffer, as they require the downstream service to be `Clone`.
