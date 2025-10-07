fn router_service(service) {
    service.map_request(Fn("process_request"));
    service.map_response(Fn("process_response"));
}

fn supergraph_service(service) {
    service.map_request(Fn("process_request"));
    service.map_response(Fn("process_response"));
}

fn execution_service(service) {
    service.map_request(Fn("process_request"));
    service.map_response(Fn("process_response"));
}

fn subgraph_service(service, subgraph) {
    service.map_request(Fn("process_request"));
    service.map_response(Fn("process_response"));
}

fn process_request(obj) {
    throw #{
        status: 403,
        body: #{
            errors: [#{
                message: "Custom error with body",
                extensions: #{
                    code: "FORBIDDEN"
                }
            }]
        }
    };
}

fn process_response(obj) {
    throw #{
        status: 403,
        body: #{
            errors: [#{
                message: "Custom error with body",
                extensions: #{
                    code: "FORBIDDEN"
                }
            }]
        }
    };
}