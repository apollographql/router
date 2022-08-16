// This example illustrates how to interact with requests and responses for
// the four different stages of processing and generate span messages
//
// The router rhai engine provides functions to allow spans to be generated.
// - span_error(msg)
// - span_warn(msg)
// - span_info(msg)
// - span_debug(msg)
// - span_trace(msg)

// Note: This is a comprehensive example which is registering request/response
// processing for all request plugin pipeline stages. This is not likely to
// be a requirement in a real application. Most applications will only register
// one or perhaps two services and may only be interested in manipulating
// requests or responses.

// At the router_service stage, register callbacks for processing requests and
// responses.
fn router_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// At the query_planner_service stage, register callbacks for processing requests and
// responses.
fn query_planner_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// At the execution_service stage, register callbacks for processing requests and
// responses.
fn execution_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// At the subgraph_service stage, register callbacks for processing requests and
// responses.
fn subgraph_service(service, subgraph) {
    print(`in subgraph ${subgraph}`);
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// Note: We can use the same functions for processing requests and responses here.
// If we wanted different behaviour for different service stages, then we would
// register different functions as required in the <stage>_service functions above.

// Generate a span for each request at this stage
fn process_request(request) {
    print(`request: ${request}`);
    span_error("this is error level span message");
    span_warn("this is warn level span message");
    span_info("this is info level span message");
    span_debug("this is debug level span message");
    span_trace("this is trace level span message");
}

// Generate a span for each response at this stage
fn process_response(response) {
    print(`response: ${response}`);
    span_error("this is error level span message");
    span_warn("this is warn level span message");
    span_info("this is info level span message");
    span_debug("this is debug level span message");
    span_trace("this is trace level span message");
}
