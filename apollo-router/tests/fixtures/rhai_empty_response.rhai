// Hooks map_response only, so the plugin reaches the point where it needs a primary response to
// hand the callback. When the response stream is empty there is nothing to hand it and the callback
// below never runs.

fn supergraph_service(service) {
    service.map_response(Fn("process_response"));
}

fn process_response(response) {
}
