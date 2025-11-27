fn subgraph_service(service, subgraph) {
    service.map_request(|request| {
        if "private_id" in request.headers {
            request.context["private_id"] = request.headers["private_id"];
        }
    });
}
