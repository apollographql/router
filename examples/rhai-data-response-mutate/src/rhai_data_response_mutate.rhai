// This example illustrates how to interact with responses to
// modify the data returned.

// At the router_service stage, register callbacks for processing 
// responses. We are using a closure here, but it could be a
// function if we chose (see execution_service).
fn router_service(service) {
    let f = |response| {
        print(response.body.data);
        print(response.body.data.me.name);
        // Let's change the name to append the service stage
        response.body.data.me.name += " and router";
        print(response.body.data.me.name);
    };
    // Map our response using our closure
    service.map_response(f);
}

// At the execution_service stage, register callbacks for processing 
// responses. We are using a function here, but we could use a
// closure if we chose (see router_service).
fn execution_service(service) {
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// At the subgraph_service stage, register callbacks for processing
// responses. (also print out the name of the subgraph)
// We are going to use a closure here to capture the name of our
// subgraph. Usually, we can just pass a Fn, like we are doing
// in the execution_service, but it's sometimes useful to pass a
// closure to include extra data.
fn subgraph_service(service, subgraph) {
    let f = |response| {
        print(response.body.data);
        print(response.body.data.me.name);
        // Let's change the name to append the subgraph name
        response.body.data.me.name += ` and subgraph(${subgraph})`;
        print(response.body.data.me.name);
    };
    // Map our response using our closure
    service.map_response(f);
}

// Note: We could use the same function for processing responses here.
// If we wanted different behaviour for different service stages, then we would
// register different functions (or use closures) as required in the <stage>_service
// functions above.

// Modify the response by adding "and execution" to the name.
fn process_response(response) {
    print(response.body.data);
    print(response.body.data.me.name);
    // Let's change the name to append the service stage
    response.body.data.me.name += " and execution";
    print(response.body.data.me.name);
}
