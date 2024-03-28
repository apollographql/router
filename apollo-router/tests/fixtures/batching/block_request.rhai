// Simple rhai script to block a request for batching testing
fn execution_service(service) {
    let request_callback = |request| {
        if request.body.query.contains("failMe") {
            throw "cancelled expected failure"
        }
    };

    service.map_request(request_callback);
}
