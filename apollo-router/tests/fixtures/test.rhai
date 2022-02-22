// This is a test used for rhai plugin

fn map_router_service_response() {
    // Header names are always in lower case
    if headers["x-custom-header"] == "CUSTOM_VALUE" {
        headers.coucou = "hello";
    }
    let value = context.test + 10;
    headers["coming_from_context"] = `value_${value}`;
    context.test = 42;
    context.addition = "Here is a new element in the context";
}

fn map_execution_service_request() {
    // Header names are always in lower case
    if headers["x-custom-header"] == "CUSTOM_VALUE" {
        headers.coucou = "hello";
    }
    context.test = 42;
    context.addition = `Here is a new element in the context with value ${context.test}`;
    context.test = 25;
    throw "An error occured";

}