fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

// Add a timestamp to context which we'll use in the response.
fn process_request(request) {
    request.context["request_start"] = Router.APOLLO_START.elapsed;
    let labels = request.context["apollo_override::unresolved_labels"];
    print(`unresolved: ${labels}`);

    let override = request.context["apollo_override::labels_to_override"];
    print(`override: ${override}`);
}

