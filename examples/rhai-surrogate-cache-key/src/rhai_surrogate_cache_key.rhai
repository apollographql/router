// This example illustrates how to interact with responses to
// modify the data returned.

// At the router_service stage, register callbacks for processing 
// responses. We are using a closure here, but it could be a
// function if we chose (see execution_service).
fn router_service(service) {
    let f = |response| {
        try {
            // Extract our surrogate key from context
            // If there is no surrogate key in our context,
            // there is no work to do.
            if response.context["surrogate-cache-key"] != () {
                let header_value = "";
                let sep = "";
                for value in response.context["surrogate-cache-key"].values() {
                    header_value += sep;
                    header_value += value;
                    sep = ",";
                }
                response.headers["surrogate-cache-key"] = header_value;
            }
        }
        catch(err)
        {
            // log any errors
            error_log(`surrogate-cache-key not created: ${err}`);
        }
    };
    // Map our response using our closure
    service.map_response(f);
}

// At the subgraph_service stage, register callbacks for processing
// responses.
// We are going to use a closure here which maintains our
// surrogate cache key.
// If we can't find headers or a cache-key, we don't want to fail,
// so catch the error and log it out.
fn subgraph_service(service, subgraph) {
    let f = |response| {
        try {
            // Try to get the current object map
            let current = response.context["surrogate-cache-key"];
            if current == () {
                // No map found. Create an empty object map
                current = #{};
            }
            // Update our object map with a key and value
            current[subgraph] = response.headers["cache-key"];
            // Upsert our context with our object map
            response.context.upsert("surrogate-cache-key", current);
        }
        catch(err)
        {
            // Log out 
            print(`cache-key not found: ${err}`);
        }
    };
    // Map our response using our closure
    service.map_response(f);
}
