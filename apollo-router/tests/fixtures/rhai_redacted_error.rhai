// A script that fails without throwing a message of its own. The failure below is not a message the
// script author chose to show a client, so it must not appear in a client-facing response.

fn router_service(service) {
    service.map_request(Fn("read_missing_header"));
}

fn supergraph_service(service) {
    service.map_request(Fn("read_missing_header"));
}

// Fails inside one of the router's own Rhai functions: reading an absent header raises an
// exception carrying no message.
fn read_missing_header(request) {
    let value = request.headers["x-header-that-is-not-there"];
}
