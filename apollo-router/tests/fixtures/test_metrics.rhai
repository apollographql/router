fn router_service(service) {
    let request_callback = |request| {
        request
    };
    service.map_request(request_callback);
}

fn supergraph_service(service) {
    let request_callback = |request| {
        request
    };
    service.map_request(request_callback);
}

fn subgraph_service(service, name) {
    let request_callback = |request| {
        request
    };
    service.map_request(request_callback);
}
