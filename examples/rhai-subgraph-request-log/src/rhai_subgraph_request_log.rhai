// This example illustrates how to interact with subgraph request
// to log various details of the request.

// At the subgraph_service stage, we log:
//  - the subgraph name
//  - various details from the originating request
//  - the same details from the subgraph request
//  Note: There is no context for the subgraph
//  request.
fn subgraph_service(service, subgraph) {
    // Create our logging closure
    let f = |request| {
        print(`subgraph: ${subgraph}`);
        print("originating request details");
        print(request.context);
        print(request.headers);
        print(request.body.query);
        print(request.body.operation_name);
        print(request.body.variables);
        print(request.body.extensions);
        print(request.uri.path);
        print(request.uri.host);
        print("subgraph request details");
        print(request.sub.headers);
        print(request.sub.body.query);
        print(request.sub.body.operation_name);
        print(request.sub.body.variables);
        print(request.sub.body.extensions);
        print(request.sub.uri.path);
        print(request.sub.uri.host);
    };
    // Map our request using our closure
    service.map_request(f);
}
