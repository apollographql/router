fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

// This will convert all cookie pairs into headers.
// If you only wish to convert certain cookies, you
// can add logic to modify the processing.
fn process_request(request) {

print(`headers: ${request.headers}`);
    // Find our cookies
    if "x-user" in request.headers {
        let user = request.headers["x-user"];
        print(`found user {user}`);

        request.context["user"] = user;
    } else {
        print("no user found");
    }
}
