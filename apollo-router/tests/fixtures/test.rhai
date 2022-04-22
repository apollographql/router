// This is a test used for rhai plugin

fn router_service_response(context) {
    context.headers["blabla"] = "here is a test";
    // Header names are always in lower case
    if context.headers["x-custom-header"] == "CUSTOM_VALUE" {
        context.headers.coucou = "hello";
    }
    let value = context.entries.test + 10;
    context.headers["coming_from_entries"] = `value_${value}`;
    context.entries.test = 42;
    context.entries.addition = "Here is a new element in the context";

    context
}

fn execution_service_request(context) {
    // Header names are always in lower case
    if context.headers["x-custom-header"] == "CUSTOM_VALUE" {
        context.headers.coucou = "hello";
    }
    context.entries.test = 42;
    context.entries.addition = `Here is a new element in the context with value ${context.entries.test}`;
    context.entries.test = 25;
    throw "An error occured";
}