# JWT authentication

DISCLAIMER: This is an example for illustrative purposes. It has not been security audited and is purely intended as an 
illustration of an approach to JWT verification via a router plugin.

Demonstrates using `checkpoint` to perform authentication and reject requests that do not pass auth.

`checkpoint` and `checkpoint_async` allow you to halt request and return immediately. This is particularly useful for authentication.

```rust
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        ServiceBuilder::new()
            .checkpoint(...) // Validation happens here
            .service(service)
            .boxed()
    }
```

