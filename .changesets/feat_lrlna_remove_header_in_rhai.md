### Add a `.remove` method for headers in Rhai

The router supports a new `.remove` method that enables users to remove headers in a Rhai script. 

For example:

``` rust
fn supergraph_service(service) {
    print("registering callbacks for operation timing");

    const request_callback = Fn("process_request");
    service.map_request(request_callback);

    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

fn process_request(request) {
    request.context["request_start"] = Router.APOLLO_START.elapsed;
}

fn process_response(response) {
    response.headers.remove("x-custom-header")
}
```

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/4632