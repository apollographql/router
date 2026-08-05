// Scripts that fail without throwing. Neither failure below is a message the script author chose
// to show a client, so neither should appear in a client-facing response.

fn router_service(service) {
    service.map_request(Fn("read_missing_header"));
}

fn supergraph_service(service) {
    service.map_request(Fn("read_missing_header"));
}

fn execution_service(service) {
    service.map_request(Fn("call_undefined_function"));
}

// Fails inside the router's own Rhai functions: reading an absent header throws an exception.
fn read_missing_header(request) {
    let value = request.headers["x-header-that-is-not-there"];
}

// Fails inside the Rhai engine itself, before any router function is reached.
fn call_undefined_function(request) {
    this_function_does_not_exist();
}
