// Call map_request with our service and pass in a string with the name
// of the function to callback
fn router_service(service) {
    print("in router_service: " + service);
    let request_callback = Fn("process_request");
    let response_callback = Fn("process_response");
    let result = map_request(service, request_callback);
    let result = map_response(service, response_callback);
    "ok?"
}

fn process_request(request) {
    print("in process_request");
    /*
    if request.operation_name != 0 {
        request.headers["X-operation-name"] = request.operation_name;
    }
    */
    print("operation name: " + request.operation_name);
    request.insert_context("this", "that");
    let value = request.get_context("this");
    print("did we find: " + value);
    print("it's a:" + type_of(value));
    switch type_of(value) {
        "string" => print("got what i wanted"),
        _ => print("hmm...")
    }
    let value = request.get_context("them");
    print("did we find: " + value);
    print("it's a:" + type_of(value));
    switch type_of(value) {
        "()" => print("Hey, I got the unit () here!"),
        _ => print("hmm...")
    }
    // throw "some pointless error"
}

fn process_response(response) {
    print("in process_response");
}
