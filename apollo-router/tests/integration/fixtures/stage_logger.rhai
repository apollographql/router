fn router_service(service) {
    service.map_request(|request| {
        print("rhai router request")
    });
    service.map_response(|response| {
        print("rhai router response")
    });
}

fn supergraph_service(service) {
    service.map_request(|request| {
        print("rhai supergraph request")
    });
    service.map_response(|response| {
        print("rhai supergraph response")
    });
}

fn execution_service(service) {
    service.map_request(|request| {
        print("rhai execution request")
    });
    service.map_response(|response| {
        print("rhai execution response")
    });
}

fn subgraph_service(service) {
    service.map_request(|request| {
        print("rhai subgraph request")
    });
    service.map_response(|response| {
        print("rhai subgraph response")
    });
}
