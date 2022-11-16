// This example illustrates how to interact with requests and responses for
// the four different stages of processing and generate log messages
//
// The router  rhai engine provides functions to allow logging into the router
// log.
// - log_error(msg)
// - log_warn(msg)
// - log_info(msg)
// - log_debug(msg)
// - log_trace(msg)

// Note: This is a comprehensive example which is registering request/response
// processing for all request plugin pipeline stages. This is not likely to
// be a requirement in a real application. Most applications will only register
// one or perhaps two services and may only be interested in manipulating
// requests or responses.

// At the supergraph_service stage, register callbacks for processing requests and
// responses.
fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    const response_callback = Fn("process_response");
    service.map_request(request_callback);
    service.map_response(response_callback);
}

// Generate a log for each request at this stage
fn process_request(request) {
    print(`before request: ${request}`);
    request.call_external("http://127.0.0.1:8081");
    print(`after request: ${request}`);
}

// Generate a log for each response at this stage
fn process_response(response) {
    print(`before response: ${response}`);
    response.call_external("http://127.0.0.1:8081");
    print(`after response: ${response}`);
}
