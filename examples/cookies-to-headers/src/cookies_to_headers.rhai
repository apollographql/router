// This will convert all cookie pairs into headers.
// If you only wish to convert certain cookies, you
// can add logic to modify the processing.
fn subgraph_service_request(context) {
    // Find our cookies
    let cookies = context.headers["cookie"].split(';');
    for cookie in cookies {
        // Split our cookies into name and value
        let k_v = cookie.split('=', 2);
        if k_v.len() == 2 {
            // trim off any whitespace
            k_v[0].trim();
            k_v[1].trim();
            // update our headers
            context.headers[k_v[0]] = k_v[1];
        }
    }
    // Always return the context 
    context
}
