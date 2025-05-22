// This is a test used to make sure each callback is called

fn router_service(service) {
    log_info("2. router_service setup");
    service.map_request(|request| {
        log_info("2. from_router_request");
    });
    service.map_response(|response| {
        log_info("2. from_router_response");
    });
}

fn supergraph_service(service) {
    log_info("2. supergraph_service setup");
    service.map_request(|request| {
        log_info("2. from_supergraph_request");
    });
    service.map_response(|response| {
        log_info("2. from_supergraph_response");
    });
}

fn execution_service(service) {
    log_info("2. execution_service setup");
    service.map_request(|request| {
        log_info("2. from_execution_request");
    });
    service.map_response(|response| {
        log_info("2. from_execution_response");
    });
}


fn subgraph_service(service, subgraph) {
    log_info("2. subgraph_service setup");
    service.map_request(|request| {
        log_info("2. from_subgraph_request");
    });
    service.map_response(|response| {
        log_info("2. from_subgraph_response");
    });
}


