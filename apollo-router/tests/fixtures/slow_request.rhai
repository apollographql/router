fn supergraph_service(service) {
    const request_callback = Fn("slow_request");
    service.map_request(request_callback);
}

fn slow_request(request) {
    let start = unix_ms_now();
    while unix_ms_now() - start < 50 {
        // Busy-loop on purpose to simulate a slow, CPU-bound Rhai script.
    }
}
