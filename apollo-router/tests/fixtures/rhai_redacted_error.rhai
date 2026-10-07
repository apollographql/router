// A script that fails inside one of the router's own Rhai functions. The error is not a message the
// script author chose to show a client, so it must not appear in a client-facing response.

fn router_service(service) {
    service.map_request(Fn("read_missing_header"));
}

fn supergraph_service(service) {
    service.map_request(Fn("read_missing_header"));
}

// Reading an absent header raises an error that names the header.
fn read_missing_header(request) {
    let value = request.headers["x-header-that-is-not-there"];
}
