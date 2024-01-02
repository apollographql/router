### Expose Context Id to Rhai scripts ([Issue #4370](https://github.com/apollographql/router/issues/4370))

We recently added an ID to Context which uniquely identifies the context for the duration of a request/response lifecycle. This is now accessible on Request or Response objects from Rhai scripts.

For example:

```rhai
// Map Request for the Supergraph Service
fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

// Generate a log for each Supergraph request with the request ID
fn process_request(request) {
    print(`request id : ${request.id}`);
}
```

Note: We have chosen to expose this Context data directly from Request/Response objects rather than on the Context object to avoid the possibility of name collisions (with "id") in the context data.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4374