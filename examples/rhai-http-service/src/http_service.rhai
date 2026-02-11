// Rhai http_service hook: runs at the raw HTTP layer in front of the router
// (before GraphQL parsing). You can read and mutate request/response headers and body.
fn http_service(service) {
    service.map_request(|request| {
        let h = request.headers;
        h["x-rhai-http-request"] = "request-mutated";
        request.headers = h;
    });
    service.map_response(|response| {
        response.headers["x-rhai-http-layer"] = "ok";
    });
}

// Rhai service_http hook: runs at the raw HTTP layer for outbound requests
// to subgraphs and connectors. You can read and mutate request/response (method, uri,
// headers, body, service_name). Use service_name to distinguish subgraphs from connectors.
fn service_http(service, service_name) {
    service.map_request(|request| {
        print("service_http: outbound request");
        let h = request.headers;
        h["x-rhai-subgraph-http-request"] = "ok";
        request.headers = h;
    });
    service.map_response(|response| {
        response.headers["x-rhai-subgraph-http-service"] = "ok";
    });
}
