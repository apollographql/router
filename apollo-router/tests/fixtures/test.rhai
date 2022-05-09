// This is a test used for rhai plugin

fn router_service(service) {
    let response = Fn("router_response");
    service.map_response(response);
}

fn router_response(response) {
    if response.headers["x-custom-header"] == "CUSTOM_VALUE" {
        response.headers["coucou"] = "hello";
    }
    let value = response.context["test"] + 10;
    response.headers["coming_from_entries"] = `value_${value}`;
    response.context["test"] = 42;
    response.context["addition"] = "Here is a new element in the context";
}

fn execution_service(service) {
    let request = Fn("execution_request");
    service.map_request(request);
}

fn execution_request(request) {
    if request.headers["x-custom-header"] == "CUSTOM_VALUE" {
        request.headers["coucou"] = "hello";
    }
    request.context["test"] = 42;
    request.context["addition"] = `Here is a new element in the context with value ${request.context["test"]}`;
    request.context["test"] = 25;
    throw "An error occured";
}
