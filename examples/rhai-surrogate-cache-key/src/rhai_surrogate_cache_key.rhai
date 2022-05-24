// This example illustrates how to interact with responses to
// modify the data returned.

// At the router_service stage, register callbacks for processing 
// responses. We are using a closure here, but it could be a
// function if we chose (see execution_service).
fn router_service(service) {
    let map_callback = |response| {
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
    service.map_response(map_callback);
}

// At the subgraph_service stage, register callbacks for processing
// responses.
// We are going to use two closures here:
//  - map_callback to map and manipulate the service response
//  - resolver to provide an upserting function for the context
//    upsert.
// If we can't find headers or a cache-key, we don't want to fail,
// so catch the error and log it out.
fn subgraph_service(service, subgraph) {
    let map_callback = |response| {
        try {
            // IMPORTANT: Take a copy of the cache-key here
            // to avoid deadlock in the closure.
            let my_cache_key = response.headers["cache-key"];
            let resolver = |current| {
                if current == () {
                    // No map found. Create an empty object map
                    current = #{};
                }
                // Update our object map with a key and value
                current[subgraph] = my_cache_key;
                return current;
            };
            // Upsert our context with our resolver
            response.context.upsert("surrogate-cache-key", resolver);
        }
        catch(err)
        {
            // Log out 
            print(`cache-key not found: ${err}`);
        }
    };
    // Map our response using our closure
    service.map_response(map_callback);
}
