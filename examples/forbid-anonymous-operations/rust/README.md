# Forbid anonymous operations

Demonstrates using `checkpoint` to prevent requests with anonymous operations.

## Usage

```bash
cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml
```

## Implementation

`checkpoint` and `checkpoint_async` allow you to halt request and return immediately. This is particularly useful for authentication.

```rust
    fn supergraph_service(
        &mut self,
        service: router::BoxService,
    ) -> router::BoxService {
        ServiceBuilder::new()
            .checkpoint(...) // Validation happens here
            .service(service)
            .boxed()
    }
```
