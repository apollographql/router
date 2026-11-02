fn supergraph_service(service) {
    const request_callback = Fn("thread_id_request");
    service.map_request(request_callback);
}

fn thread_id_request(request) {
    request.context["thread_id"] = current_thread_id();
}
