fn supergraph_service(service) {
    service.map_request(|request| {
        request.context["integration_test_entity_cache_keys"] = [];
    });
    service.map_response(|response| {
        let keys = response.context["integration_test_entity_cache_keys"];
        response.body.extensions["cache_keys"] = keys;
    });
}

fn subgraph_service(service, subgraph) {
    service.map_request(|request| {
        let keys = request.context["integration_test_entity_cache_keys"].push(request.subgraph_request_id);
    });
}