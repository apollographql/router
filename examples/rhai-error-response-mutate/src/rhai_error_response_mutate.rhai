// This example illustrates how to interact with responses to
// modify the errors returned.

// At the router_service stage, register callbacks for processing 
// responses. We are using a closure here, but it could be a
// function if we chose.
fn router_service(service) {
    let f = |response| {
        // Log out the errors we receive
        print(response.body.errors);

        // Remove all instances of ".prod.svc.cluster.local" from the message if present
        response.body.errors = response.body.errors.map(|v| {
            v.message.remove("mysystem.prod.svc.cluster.local");
            // Or if you wanted to replace it with "redacted"...
            // v.message.replace("mysystem.prod.svc.cluster.local", "redacted");
            v
        });

        // Let's add an extra error
        // Define a minimal error. You need at least:
        //   - a message string
        //   - an empty locations array
        // extensions are optional. Added here to illustrate syntax.
        const error_to_add = #{
            message: "this is an added error",
            locations: [],
            extensions:
                #{
                field_1: "field 1",
                field_2: "field_2"
                }
        };
        response.body.errors += error_to_add;

        // Let's terminate early because we have errors
        if response.body.errors.len() > 0 {
            print("terminating response processing early because we have errors");
            return;
        }
        // If we didn't have errors we'd carry on processing...
    };
    // Map our response using our closure
    service.map_response(f);
}
