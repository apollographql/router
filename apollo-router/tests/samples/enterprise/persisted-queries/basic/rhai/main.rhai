fn router_service(service) {
    service.map_request(|request| {
        if (request.headers.contains("plugin-client-name")) {
            request.context["apollo_persisted_queries::client_name"] = request.headers["plugin-client-name"];
        }
    });
}
