// Test script for http_service hook: map_request and map_response at the HTTP layer.
fn http_service(service) {
    service.map_request(|request| {
        // Optionally mutate request (e.g. add a header for downstream to see)
        let h = request.headers;
        h["x-rhai-http-request"] = "request-mutated";
        request.headers = h;
    });
    service.map_response(|response| {
        response.headers["x-rhai-http-layer"] = "ok";
    });
}
