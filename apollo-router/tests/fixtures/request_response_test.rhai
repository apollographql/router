// These tests ensure that we can process request/responses and that we are receiving the
// correct values provided in the rust layer.
// If any of the tests fail, the thrown error will cause the respective rust
// unit test to fail.

fn process_common_request(check_context_and_method, request) {
    if check_context_and_method {
        if request.context.entries != () {
            throw(`context entries: expected: (), actual: ${request.context.entries}`);
        }
        if request.method != "GET" {
            throw(`query: expected: "GET", actual: ${request.method}`);
        }
    }
    if request.body.operation_name != () {
        throw(`operation name: expected: canned, actual: ${request.body.operation_name}`);
    }
    if request.body.query != () {
        throw(`query: expected: (), actual: ${request.body.query}`);
    }
    if request.body.variables != #{} {
        throw(`query: expected: #{}, actual: ${request.body.variables}`);
    }
    if request.body.extensions != #{} {
        throw(`query: expected: #{}, actual: ${request.body.extensions}`);
    }
    if request.uri.host != () {
        throw(`query: expected: (), actual: ${request.uri.host}`);
    }
    if request.uri.path != "/" {
        throw(`query: expected: "/", actual: ${request.uri.path}`);
    }
}

fn process_supergraph_request(request) {
    if request.context.entries != () {
        throw(`context entries: expected: (), actual: ${request.context.entries}`);
    }
    if request.headers["content-type"] != "application/json" {
        throw(`header["content-type"]: expected: "application/json", actual: ${request.headers.content-type}`);
    }
    if request.method.to_string() != "POST" {
        throw(`query: expected: "POST", actual: ${request.method}`);
    }
    if request.body.operation_name != "canned" {
        throw(`operation name: expected: canned, actual: ${request.body.operation_name}`);
    }
    let expected_query = `

            query TopProducts($first: Int) {
                topProducts(first: $first) {
                    upc
                    name
                    reviews {
                        id
                        product { name }
                        author { id name }
                    }
                }
            }
        `;
    if request.body.query != expected_query {
        throw(`query: expected: ${expected_query}, actual: ${request.body.query}`);
    }
    if request.body.variables != #{"first": 2} {
        throw(`query: expected: #{"first": 2}, actual: ${request.body.variables}`);
    }
    if request.body.extensions != #{} {
        throw(`query: expected: #{}, actual: ${request.body.extensions}`);
    }
    if request.uri.host != "default" {
        throw(`query: expected: "default", actual: ${request.uri.host}`);
    }
    if request.uri.path != "/" {
        throw(`query: expected: "/", actual: ${request.uri.path}`);
    }
}

fn process_execution_request(request) {
    process_common_request(true, request);
    if request.query_plan != "" {
        throw(`query: expected: (), actual: ${request.query_plan}`);
    }
}

fn process_subgraph_request(request) {
    process_common_request(true, request);
    // subgraph doesn't have a context member
    process_common_request(false, request.subgraph);
}

fn process_common_response(response) {
    if response.context.entries != () {
        throw(`context entries: expected: (), actual: ${response.context.entries}`);
    }
    if response.body.label != () {
        throw(`label: expected: (), actual: ${response.body.label}`);
    }
    if response.body.data != () {
        throw(`query: expected: (), actual: ${response.body.data}`);
    }
    if response.body.errors != [] {
        throw(`query: expected: [], actual: ${response.body.errors}`);
    }
    if response.body.extensions != #{} {
        throw(`query: expected: #{}, actual: ${response.body.extensions}`);
    }
}

fn process_supergraph_response(response) {
    process_common_response(response);
}

fn process_execution_response(response) {
    process_common_response(response);
}

fn process_subgraph_response(response) {
    process_common_response(response);
}

fn process_subgraph_response_om_forbidden(response) {
    throw #{
        status: 403,
        message: "I have raised a 403"
    };
}

fn process_subgraph_response_om_forbidden_graphql(response) {
    throw #{
        status: 403,
        body: #{
            errors: [#{
                message: `I have raised a 403`,
                extensions: #{
                    code: "ACCESS_DENIED"
                }
            }]
        }
    };
}


fn process_subgraph_response_om_200_graphql(response) {
    throw #{
        status: 200,
        body: #{
            data: #{
                name: "Ada Lovelace"
            }
        }
    };
}
fn process_subgraph_response_string(response) {
    throw "I have raised an error";
}

fn process_subgraph_response_om_ok(response) {
    throw #{
        status: 200,
        message: "I have raised a 200"
    };
}

fn process_subgraph_response_om_missing_message(response) {
    throw #{
        status: 400,
    };
}

fn process_supergraph_response_is_primary(response) {
    if !response.is_primary() {
        throw #{
            status: 500,
            message: "should be primary"
        };
    }
}

fn process_execution_response_is_primary(response) {
    if !response.is_primary() {
        throw #{
            status: 500,
            message: "should be primary"
        };
    }
}

fn process_supergraph_deferred_response_is_not_primary(response) {
    if response.is_primary() {
        throw #{
            status: 500,
            message: "should not be primary"
        };
    }
}

fn process_execution_deferred_response_is_not_primary(response) {
    if response.is_primary() {
        throw #{
            status: 500,
            message: "should not be primary"
        };
    }
}

