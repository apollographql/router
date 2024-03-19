// Simple rhai script to block a request for batching testing
fn execution_service(service) {
    let request_callback = |request| {
        if request.body.query.contains("entryB") {
            throw "Cancelling request for subgraph B"
        }
    };

    service.map_request(request_callback);
}
