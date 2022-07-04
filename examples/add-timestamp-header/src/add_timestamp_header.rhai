// The apollo rhai engine provides a global constant "apollo_start" which can
// be used as a time base for timing operations.

// Call map_request/map_response for our service and pass in strings with the names
// of the functions to callback
fn router_service(service) {
    print("registering callbacks for operation timing");

    const request_callback = Fn("process_request");
    service.map_request(request_callback);

    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// Add a timestamp to context which we'll use in the response.
fn process_request(request) {
    request.context["request_start"] = apollo_start.elapsed;
}

// Calculate elapsed time and add that to the response as a header
fn process_response(response) {
    let elapsed = apollo_start.elapsed - response.context["request_start"];
    print(`elapsed: ${elapsed}`);

    // Add the elapsed time as a header to the response
    response.headers["x-elapsed-time"] = elapsed.to_string();
}
