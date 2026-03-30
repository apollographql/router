fn supergraph_service(service) {
    let request_callback = |request| {
        throw "intentional test failure";
    };
    service.map_request(request_callback);
}
