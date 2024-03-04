fn supergraph_service(service) {
    let response = Fn("supergraph_response")  // There should be a semicolon here
    service.map_response(response);
}
