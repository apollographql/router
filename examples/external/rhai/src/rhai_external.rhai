// This is an example which illustrates the proposed new external processing
// capability for rhai. The key point is that the map_request() method is extended
// with an option url. If the optional url is present, then this url is called
// for external processing before the rhai callback is invoked.
//
// Data can be optionally modified at the external processor as follows:
//  - body
//  - headers
//  - context
//
// Note: At the subgraph stage, body and headers cannot be modified because they
// are shared immutably across all subgraphs.
//
// Note: Responses are not currently implemented because I'm not sure if it's
// required functionality and I thought I'd get some feedback on the current
// approach before I looked at that.
//
// Note: Chaining maps on the service isn't implemented yet. I need to re-structure
// some macros to get that working.


// At the supergraph_service stage, register callbacks for processing requests and
// responses. For fun, map the request twice, once externally and then again
// internally. If required, e.g. for fan-out processing, the map_request could
// be made as many times as required for the service.
fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    const response_callback = Fn("process_response");
    service.map_request(request_callback, "http://127.0.0.1:3000");
    service.map_response(response_callback);
}

// Generate a log for each request at this stage
fn process_request(request) {
    print(`request query: ${request.body.query}`);
}

// Generate a log for each response at this stage
fn process_response(response) {
    print(`response data: ${response.body.data}`);
}
