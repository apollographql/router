// Edits request variables, then throws before the request reaches the next service.

fn supergraph_service(service) {
    service.map_request(|request| {
        request.body.variables.edited = true;
        throw "request failure";
    });
}
