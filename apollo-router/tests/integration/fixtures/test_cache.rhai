fn subgraph_service(service, subgraph) {
    log_info("1. subgraph_service setup");
    service.map_request(|request| {
        print(`${request.headers}`);
        if "private_id" in request.headers {
            request.context["private_id"] = request.headers["private_id"];
        }
    });
}
