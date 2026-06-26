fn router_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

fn process_request(request) {
    request.context["apollo_telemetry::client_name"] = "deprecated_client";
}
