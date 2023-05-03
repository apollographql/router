// This example illustrates how to interact with responses to
// modify the errors returned.

// At the supergraph_service stage, register callbacks for processing 
// responses. We are using a closure here, but it could be a
// function if we chose.
fn supergraph_service(service) {
    let f = |request| {
        // Log out the errors we receive
        print(request.body);

        throw #{
            status: 401, // or whatever statuscode you wanna use
            body: #{
                errors: [#{
                    message: `Access denied`,
                    extensions: #{
                        code: "UNAUTHENTICATED"
                    }
                }]
            }
        };
    };
    // Map our response using our closure
    service.map_request(f);
}