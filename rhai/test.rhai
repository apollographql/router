// At the supergraph_service stage responses.
fn router_service(service) {
    const response_callback = Fn("process_response"); // This is standard Rhai functionality for creating a function pointer
    service.map_response(response_callback); // Register the response callback
}

fn process_response(response) {
    if response.is_primary() {
        let len = response.body.errors.len();
        if response.body.errors > 0 {
            log_warn("error");

        }
        log_warn(len);
    }
}