// Call map_request with our service and pass in a string with the name
// of the function to callback
fn router_service(service) {
    print("registering request callback to add operation name to a header");
    const request_callback = Fn("process_request");
    map_request(service, request_callback);
}

// Process the provided request to extract the operation name and add a header
fn process_request(request) {
    print("adding operation name to header");

    // We may not have an operation name, so check if we do
    if request.body.operation_name != () {
        request.headers["x-operation-name"] = request.body.operation_name;
    }
}
