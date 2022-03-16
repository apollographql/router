fn subgraph_service_request(context) {
    let cookies = context.headers["Cookie"].split(';');
    for cookie in cookies {
        let k_v = cookie.split('=', 2);
        let key = k_v.get(0);
        let value = k_v.get(1);
        context.headers[key] = value;
    }
    context
}
