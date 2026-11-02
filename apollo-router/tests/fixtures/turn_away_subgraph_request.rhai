// Turns away subgraph requests for the `TurnAway` operation with a `401`, and sends the rest on.

fn subgraph_service(service, subgraph) {
    service.map_request(Fn("turn_away"));
}

fn turn_away(request) {
    if request.subgraph.body.operation_name == "TurnAway" {
        throw #{
            status: 401,
            message: "turned away by the script"
        };
    }
}
