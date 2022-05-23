// This example illustrates how to interact with responses to
// modify the data returned.

// At the router_service stage, register callbacks for processing 
// responses. We are using a closure here, but it could be a
// function if we chose (see execution_service).
fn router_service(service) {
    let f = |response| {
        print(`COMPOSITE MAP: ${response.context}`);
        try {
            // Extract our surrogate key from context
            // If there is no surrogate key in our context,
            // this will fail and so we catch the error and log it.
            // response.headers["surrogate-cache-key"] = response.context["surrogate-cache-key"];
        }
        catch(err)
        {
            // Log out 
            print(`surrogate-cache-key not found: ${err}`);
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
        print(`IS subgraph SHARED?: ${subgraph.is_shared()}`);
        // Insert a fake header TODO: REMOVE LATER
        response.headers["cache-key"] = `${subgraph}`;
        print(`HEADERS ARE NOW: ${response.headers["cache-key"]}`);
        try {
            print(`Setting subgraph cache key for ${subgraph}`);
            print(`CHECKING EXISTING SURROGATE CACHE KEY IS: ${response.context["surrogate-cache-key"]}`);
            let current = response.context["surrogate-cache-key"];
            print(`AFTER Setting subgraph cache key for ${subgraph}`);
            if current == () {
                print("CREATING NEW current");
                current = #{};
                current[subgraph] = response.headers["cache-key"];
                // current.set(`${subgraph}`, response.headers["cache-key"]);
            } else {
                print(`UPDATING EXISTING current: ${current}`);
                let latest = #{};
                print(`UPDATING EXISTING latest: ${latest}`);
                print(`MY RESPONSE HEADER: ${response.headers}`);
                latest.set(`${subgraph}`, response.headers["cache-key"]);
                current += latest;
            }
            print(`UPSERTING current: ${current}`);
            response.context.upsert("surrogate-cache-key", current);
            // response.context["surrogate-cache-key"] = current;
        }
        catch(err)
        {
            // Log out 
            print(`cache-key not found: ${err}`);
        }
    };
    print(`IS subgraph SHARED?: ${subgraph.is_shared()}`);
    // Map our response using our closure
    service.map_response(f);
}
