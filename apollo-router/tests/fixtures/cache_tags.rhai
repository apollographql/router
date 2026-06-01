fn supergraph_service(service) {
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

fn process_response(response) {
    let tags = response.context[Router.APOLLO_RESPONSE_CACHE_TAGS_KEY];
    if tags != () {
        let header = "";
        for tag in tags {
            if header != "" { header += ","; }
            header += "acme:";
            header += tag;
        }
        if header != "" {
            response.headers["cache-tag"] = header;
        }
    }
}
