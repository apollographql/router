### Move authorization out of the authentication plugin 

*Breaking change*: this changes the JWT plugin's behaviour, it only rejects requests with invalid tokens, but otherwise lets everything go through

Now the authentication plugin only rejects invalid tokens (bad signature, expired, etc). Authorization must be handled later.
As an example, a rhai script can be used to refuse all unauthenticated requests, like this:

```
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
```

This also adds support for the `extensions` field in GraphQL errors generated from rhai

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2708