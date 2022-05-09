// Call map_request with our service and pass in a string with the name
// of the function to callback
fn subgraph_service(service) {
    print("registering request callback");
    let request_callback = Fn("process_request");
    map_request(service, request_callback);
}

// This will convert all cookie pairs into headers.
// If you only wish to convert certain cookies, you
// can add logic to modify the processing.
fn process_request(request) {
    print("adding cookies as headers");

    // Find our cookies
    let cookies = request.headers["cookie"].split(';');
    for cookie in cookies {
        // Split our cookies into name and value
        let k_v = cookie.split('=', 2);
        if k_v.len() == 2 {
            // trim off any whitespace
            k_v[0].trim();
            k_v[1].trim();
            // update our headers
            // Note: we must update sub_headers, since we are
            // setting a header in our sub graph request
            request.sub_headers[k_v[0]] = k_v[1];
        }
    }
}
