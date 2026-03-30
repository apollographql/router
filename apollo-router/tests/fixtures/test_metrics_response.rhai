fn subgraph_service(service, name) {
    let response_callback = |response| {
        response
    };
    service.map_response(response_callback);
}

fn supergraph_service(service) {
    let response_callback = |response| {
        response
    };
    service.map_response(response_callback);
}
