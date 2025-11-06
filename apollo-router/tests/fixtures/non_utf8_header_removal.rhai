fn supergraph_service(service) {
    print("registering callbacks for non-UTF-8 header removal test");

    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

fn process_response(response) {
    // This will fail when trying to remove a non-UTF-8 header
    // because the remove function calls .to_str() on the header value
    response.headers.remove("x-binary-header")
}
