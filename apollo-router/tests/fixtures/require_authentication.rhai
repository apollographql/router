fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

// This will examine our context to determine if our user is authenticated.
fn process_request(request) {
    let claims = request.context[Router.APOLLO_AUTHENTICATION_JWT_CLAIMS];
    if claims == () {
        throw #{
            status: 401,
            body: #{
                errors: [#{
                    message: "The request is not authenticated",
                    extensions: #{
                        code: "AUTH_ERROR"
                    }
                }]
            }
        };
    }
    // We are happy we are authenticated.
}