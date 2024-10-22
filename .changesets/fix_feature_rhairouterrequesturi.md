### Support URI and method properties for router request in Rhai ([PR #6147](https://github.com/apollographql/router/pull/6147))

The router now supports accessing `request.uri` and `request.method` properties from custom Rhai scripts.

Previously, when trying to access `request.uri` and `request.method` on a router request in Rhai, the router would return error messages stating the properties were undefined.

An example Rhai script using these properties:

```rhai
fn router_service(service) {
  let router_request_callback = Fn("router_request_callback");
  service.map_request(router_request_callback);
}

fn router_request_callback (request) {
  log_info(`Router Request... Host: ${request.uri.host}, Path: ${request.uri.path}`);
}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6114
