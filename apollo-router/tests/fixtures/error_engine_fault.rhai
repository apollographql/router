fn router_service(service) {
    service.map_request(Fn("process_request"));
}

fn supergraph_service(service) {
    service.map_request(Fn("process_request"));
}

fn execution_service(service) {
    service.map_request(Fn("process_request"));
}

fn subgraph_service(service, subgraph) {
    service.map_request(Fn("process_request"));
}

fn process_request(obj) {
    // No script can reach this error via `throw`: it comes from calling a function that
    // doesn't exist, which the engine reports as `EvalAltResult::ErrorFunctionNotFound`.
    this_function_does_not_exist();
}
