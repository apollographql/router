// This example illustrates how to interact with responses to
// modify the data returned.

// At the execution_service stage, register callbacks for processing 
// responses.
fn execution_service(service) {
    let response_callback = Fn("process_response");
    service.map_response(response_callback);
}

// At the subgraph_service stage, register callbacks for processing
// responses. (also print out the name of the subgraph)
// We are going to use a closure here to capture the name of our
// subgraph. Usually, we can just pass a Fn (like we are doing
// in the execution_service, but it's sometimes useful to pass a
// closure to include extra data like we are doing here.)
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

// Note: We can use the same functions for processing responses here.
// If we wanted different behaviour for different service stages, then we would
// register different functions as required in the <stage>_service functions above.

// Generate a log for each response at this stage
fn process_response(response) {
    print(response.body.data);
    print(response.body.data.me.name);
    // Let's change the name to append the service stage
    response.body.data.me.name += " and execution";
    print(response.body.data.me.name);
}
