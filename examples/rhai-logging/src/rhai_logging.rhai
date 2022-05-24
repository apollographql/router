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

// Generate a log for each request at this stage
fn process_request(request) {
    print(`request: ${request}`);
    log_error("this is error level log message");
    log_warn("this is warn level log message");
    log_info("this is info level log message");
    log_debug("this is debug level log message");
    log_trace("this is trace level log message");
}

// Generate a log for each response at this stage
fn process_response(response) {
    print(`response: ${response}`);
    log_error("this is error level log message");
    log_warn("this is warn level log message");
    log_info("this is info level log message");
    log_debug("this is debug level log message");
    log_trace("this is trace level log message");
}
