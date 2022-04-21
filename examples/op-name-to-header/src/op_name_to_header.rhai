// Call map_request with our service and pass in a string with the name
// of the function to callback
fn router_service(service) {
    print("in router_service: " + service);
    let result = map_request(service, "process_request");
    "ok?"
}

fn process_request(request) {
    print("in process_request");
    let context = request.context;
    print("request context: " + context);
    request.context = context;
    request
}
