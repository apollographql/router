// This example illustrates different patterns for throwing errors
// to control request flow and terminate requests at various stages.

// Example 1: Terminate at supergraph service (affects entire request)
fn supergraph_service(service) {
    let f = |request| {
        // Log out the request body we receive
        print(request.body);

        // Example: Check for authentication
        let headers = request.headers;
        if !headers.contains("authorization") {
            throw #{
                status: 401,
                body: #{
                    errors: [#{
                        message: `Access denied - missing authorization header`,
                        extensions: #{
                            code: "UNAUTHENTICATED"
                        }
                    }]
                }
            };
        }
    };
    service.map_request(f);
}

// Example 2: Conditionally skip specific subgraphs while maintaining query shape
// This is useful when you want to skip certain subgraphs based on conditions
// but still allow other subgraphs to execute normally in federated queries.
fn subgraph_service(service, subgraph) {
    service.map_request(|request| {
        // Example: Skip the "users" subgraph if user context is missing
        if subgraph == "users" {
            let headers = request.headers;
            if !headers.contains("x-user-id") {
                // Throw a proper GraphQL response to skip this subgraph
                // while maintaining the overall query structure
                throw #{
                    status: 200, // Use 200 since this is a valid GraphQL response
                    body: #{
                        data: #{}, // Important: Use empty object, not null
                        errors: [#{
                            message: `Subgraph '${subgraph}' skipped due to missing user context`,
                            extensions: #{
                                code: "SUBGRAPH_SKIPPED"
                            }
                        }]
                    }
                };
            }
        }
    });
}