// Call map_request with our service and pass in a string with the name
// of the function to callback
fn router_service(service) {
    print("This is a message");
    print("in router_service: " + service);
    let request_callback = Fn("process_request");
    let response_callback = Fn("process_response");
    let result = map_request(service, request_callback);
    let result = map_response(service, response_callback);
    "ok?"
}

fn process_request(request) {
    print("in process_request: " + request);

    print("*** Let's test out some header manipulation stuff ***");
    print("*** Print the headers ***");
    print("original headers: \n" + request.headers);
    print("*** Loop over our headers and Print pairs and also print each name and value as fields ***");
    for pair in request.headers {
        print("\tpair: " + pair);
        print("\tname: " + pair.name + " value: " + pair.value);
    }

    print("*** Add a header called 'my_header' with value 'something' ***");
    request.headers["my_header"] = "something";

    print("*** Print our modified headers ***");
    print("modified headers: \n" + request.headers);

    print("*** We've updated our request. Every time we get headers, we'll have our new header in there ***");
    print("final headers: \n" + request.headers);

    if request.operation_name != () {
        request.headers["x-operation-name"] = request.operation_name;
    }

    print("*** MORE TESTING OF STUFF, operation names, context, ... ***");
    print("operation name: " + request.operation_name);
    request.context["this"] = "that";
    let value = request.context["this"];
    print("did we find: " + value);
    print("it's a:" + type_of(value));
    switch type_of(value) {
        "string" => print("got what i wanted"),
        _ => print("hmm...")
    }
    let value = request.context["them"];
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
    print("response headers: " + response.headers);
    response.headers["me"] = "whatever";
    print("response headers: " + response.headers);
}
