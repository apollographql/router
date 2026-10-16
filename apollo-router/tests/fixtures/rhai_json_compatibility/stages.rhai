// Edits every JSON-bearing field at every stage that exposes one.

fn supergraph_service(service) {
    service.map_request(Fn("edit_request"));
    service.map_response(Fn("edit_response"));
}

fn execution_service(service) {
    service.map_request(Fn("edit_request"));
    service.map_response(Fn("edit_response"));
}

fn subgraph_service(service, subgraph) {
    service.map_request(Fn("edit_subgraph_request"));
    service.map_response(Fn("edit_response"));
}

fn edit_request(request) {
    request.body.variables.added = "variable";
    request.body.extensions.added = "extension";
}

fn edit_subgraph_request(request) {
    request.subgraph.body.variables.added = "variable";
    request.subgraph.body.extensions.added = "extension";
    // The originating request is read-only at the subgraph stage, so this edit is discarded.
    request.body.variables.ignored = true;
}

fn edit_response(response) {
    response.body.data.label = response.body.label;
    response.body.errors[0].message += " (edited)";
    response.body.errors.push(#{ message: "added" });
    response.body.extensions.edited = true;
}
