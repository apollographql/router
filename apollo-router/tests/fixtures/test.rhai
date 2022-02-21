// This is a test used for rhai plugin

fn router_service() {
    // Header names are always in lower case
    if headers["x-custom-header"] == "CUSTOM_VALUE" {
        headers.coucou = "hello";
    }
    let value = context.test + 10;
    headers["coming_from_context"] = `value_${value}`;
    context.test = 42;
    context.addition = "Here is a new element in the context";
}

fn execution_service() {
    // Header names are always in lower case
    if headers["x-custom-header"] == "CUSTOM_VALUE" {
        headers.coucou = "hello";
    }
    context.test = 42;
    context.addition = `Here is a new element in the context with value ${context.test}`;
    context.test = 25;
}