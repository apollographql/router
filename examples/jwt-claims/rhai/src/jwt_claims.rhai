// Call map_request with our service and pass in a string with the name
// of the function to callback
fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

// This will examine our context for any JWT claims that our
// JWT validation has extracted.
//
// We can take actions based on the evaluation of those claims.
//
// In this example we return UNAUTHORISED if there are no claims
// or "iss" claim
// is either:
//  - not present
//  - not equal to "issuer"
//
fn process_request(request) {
    try {
        let claims = request.context[Router.APOLLO_AUTHENTICATION_JWT_CLAIMS];
        if claims == () {
            throw #{
                status: 401,
                message: "No claims presented in this request"
            };
        }
        if !claims.contains("iss") {
            throw #{
                status: 401,
                message: "No issuer presented in this request"
            };
        }
        if claims["iss"] != "https://idp.local" {
            throw #{
                status: 401,
                message: `Issuer ${claims.iss} is not accepted here`
            };
        }
        // We are happy we have claims from the correct idp, do something
        // with them. In this case, we'll just print them out.
        print(`claims: ${claims}`);
    }
    catch(err)
    {
        // log any errors
        log_error(`Error during claims processing: ${err}`);
        throw err;
    }
}
