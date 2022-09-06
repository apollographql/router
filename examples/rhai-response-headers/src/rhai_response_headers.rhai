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
    let f = |response| {
        log_info(`supergraph headers: ${response.headers}`);
    };
    // Map our response using our closure
    service.map_response(f);
}

// At the execution_service stage, register callbacks for processing requests and
// responses.
fn execution_service(service) {
    let f = |response| {
        log_info(`execution headers: ${response.headers}`);
    };
    // Map our response using our closure
    service.map_response(f);
}

// At the subgraph_service stage, register callbacks for processing requests and
// responses.
fn subgraph_service(service, subgraph) {
    log_info(`in subgraph ${subgraph}`);
    let f = |response| {
        log_info(`subgraph headers: ${response.headers}`);
        // response.headers["vary"] = "potato";
        response.headers["vairy"] = "potato";
    };
    // Map our response using our closure
    service.map_response(f);
}
