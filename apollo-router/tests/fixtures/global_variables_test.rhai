// This test ensures that we can access router global variables
// It doesn't test the values of the variables, apart from JWT CLAIMS
fn process_router_global_variables() {

    return Router.APOLLO_SDL != () && Router.APOLLO_START != () && Router.APOLLO_AUTHENTICATION_JWT_CLAIMS == "apollo_authentication::JWT::claims";
}

