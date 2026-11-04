// Callbacks at every stage that never touch a JSON-bearing field.

fn supergraph_service(service) {
    service.map_request(|request| { request.headers["x-touched"] = "request"; });
    service.map_response(|response| { response.context["touched"] = response.is_primary(); });
}

fn execution_service(service) {
    service.map_request(|request| { request.headers["x-touched"] = "request"; });
    service.map_response(|response| { response.context["touched"] = response.is_primary(); });
}

fn subgraph_service(service, subgraph) {
    service.map_request(|request| { request.subgraph.headers["x-touched"] = "request"; });
    service.map_response(|response| { response.context["touched"] = response.status_code.to_string(); });
}
