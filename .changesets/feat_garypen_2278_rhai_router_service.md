### Provide a rhai interface to the router service ([Issue #2278](https://github.com/apollographql/router/issues/2278))

Adds `Rhai` support for the `router_service`.

It is now possible to interact with requests and responses at the `router_service` level from `Rhai`. The functionality is very similar to that provided for interacting with existing services, for example `supergraph_service`. For instance, you may map requests and responses as follows:

```rust
fn router_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

```
The main difference from existing services is that the router_service is dealing with HTTP Bodies, not well formatted GraphQL objects. This means that the `Request.body` or `Response.body` is not a well structured object that you may interact with, but is simply a String.

This makes it more complex to deal with Request and Response bodies with the tradeoff being that a script author has more power and can perform tasks which are just not possible within the confines of a well-formed GraphQL object.

This simple example, simply logs the bodies:

```rust
// Generate a log for each request at this stage
fn process_request(request) {
    print(`body: ${request.body}`);
}

// Generate a log for each response at this stage
fn process_response(response) {
    print(`body: ${response.body}`);
}
```

This PR also introduces two new Rhai functions:

```rust
json_encode(Object)
json_decode(String) -> Object

```
Which will respectively encode a `Rhai` Object or decode a JSON string into a `Rhai` Object. These functions may be helpful when dealing with String bodies which represent encoded JSON objects.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3234
