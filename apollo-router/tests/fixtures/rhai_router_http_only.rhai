// Rhai script that only defines router_http (no other services).
// Used to test that the router_http hook runs and can read/write context.
fn router_http(service) {
    service.map_request(|request| {
        request.context.upsert("rhai_router_http_ran", |v| true);
        request
    });
    service.map_response(|response| {
        response.context.upsert("rhai_router_http_ran", |v| true);
        response
    });
}
