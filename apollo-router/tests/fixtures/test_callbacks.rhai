// This is a test used to make sure each callback is called

fn router_service(service) {
    log_info("router_service setup");
    service.map_request(|request| {
        log_info("from_router_request");
    });
    service.map_response(|response| {
        log_info("from_router_response");
    });
}

fn query_planner_service(service) {
    log_info("query_planner_service setup");
    service.map_request(|request| {
        log_info("from_query_planner_request");
    });
    service.map_response(|response| {
        log_info("from_query_planner_response");
    });
}

fn execution_service(service) {
    log_info("execution_service setup");
    service.map_request(|request| {
        log_info("from_execution_request");
    });
    service.map_response(|response| {
        log_info("from_execution_response");
    });
}


fn subgraph_service(service, subgraph) {
    log_info("subgraph_service setup");
    service.map_request(|request| {
        log_info("from_subgraph_request");
    });
    service.map_response(|response| {
        log_info("from_subgraph_response");
    });
}


