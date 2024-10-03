### Add uri and method properties on router request in rhai ([PR #6114](https://github.com/apollographql/router/pull/6114))

Previously, when trying to access `request.uri` and `request.method` on a Router Request in Rhai, Router would error saying the properties are undefined.

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
