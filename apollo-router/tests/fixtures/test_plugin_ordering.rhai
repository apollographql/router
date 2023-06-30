fn supergraph_service(service) {
    service.map_request(|request| {
        request.context["ordering-trace"].push("supergraph_service Rhai map_request")
    });
    service.map_response(|response| {
        response.context["ordering-trace"].push("supergraph_service Rhai map_response")
    });
  }