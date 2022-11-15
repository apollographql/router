// This example illustrates how reject anonymous operations (operations without a name defined).

// At the supergraph_service stage, register callbacks for processing 
// responses. 
fn supergraph_service(service){
    const request_callback = Fn("process_request");
    service.map_request(request_callback)
}

// For each request, we'll check for the existence of an operation name, 
// exposed by the request.body.operation_name variable
// If missing, we throw an error; otherwise, the request proceeds as normal.
fn process_request(request){
    if request.body.operation_name == () {
        throw "Anonymous operations are not allowed";
    }
}