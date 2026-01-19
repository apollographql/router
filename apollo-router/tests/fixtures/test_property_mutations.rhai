// Test that property mutations work correctly in contexts where they should

fn test_supergraph_header_mutation(request) {
    // Should be able to mutate headers in supergraph context
    request.headers["x-test-mutation"] = "mutated";

    if request.headers["x-test-mutation"] != "mutated" {
        throw(`Failed to mutate header in supergraph context`);
    }
}

fn test_supergraph_body_mutation(request) {
    // Should be able to mutate body in supergraph context
    let new_body = request.body;
    new_body.operation_name = "mutated_operation";
    request.body = new_body;

    if request.body.operation_name != "mutated_operation" {
        throw(`Failed to mutate body in supergraph context`);
    }
}

fn test_execution_header_mutation(request) {
    // Should be able to mutate headers in execution context
    request.headers["x-execution-test"] = "execution";

    if request.headers["x-execution-test"] != "execution" {
        throw(`Failed to mutate header in execution context`);
    }
}

fn test_router_header_mutation(request) {
    // Should be able to mutate headers in router context
    request.headers["x-router-test"] = "router";

    if request.headers["x-router-test"] != "router" {
        throw(`Failed to mutate header in router context`);
    }
}

fn test_subgraph_read_only_headers(request) {
    // Should be able to READ headers in subgraph context
    let method = request.method;
    if method == () {
        throw(`Failed to read method in subgraph context`);
    }

    // Should be able to access URI
    let uri = request.uri;
    if uri.path == () {
        throw(`Failed to read URI in subgraph context`);
    }

    // Should be able to access body
    let body = request.body;
    if body == () {
        throw(`Failed to read body in subgraph context`);
    }
}

fn test_subgraph_property_chain_with_split(request) {
    // Verify property chains work on read-only headers in subgraph context
    let cookies = request.headers["cookie"].split(';');
    if cookies.len() != 3 {
        throw(`Expected 3 cookies, got ${cookies.len()}`);
    }
}

fn test_subgraph_property_chain_with_trim(request) {
    // Verify trim() works on read-only headers in subgraph context
    let auth = request.headers["auth"];
    if auth.is_empty() {
        throw(`Header not found or empty`);
    }
    let trimmed = auth.trim();
}

fn test_complex_property_chain(request) {
    // Verify chained operations (split, index, trim) work on read-only headers in subgraph context
    let cookies = request.headers["cookie"].split(';');
    let first = cookies[0];
    let first_cookie = first.trim();
}
