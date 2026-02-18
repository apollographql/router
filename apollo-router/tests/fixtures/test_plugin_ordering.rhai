fn push_trace(context, entry) {
    context.upsert("ordering-trace", |trace| {
        let t = if type_of(trace) == "array" { trace } else { [] };
        t.push(entry);
        t
    })
}

fn router_http(service) {
    service.map_request(|request| {
        push_trace(request.context, "router_http Rhai map_request")
    });
    service.map_response(|response| {
        push_trace(response.context, "router_http Rhai map_response")
    });
}

fn supergraph_service(service) {
    service.map_request(|request| {
        push_trace(request.context, "supergraph_service Rhai map_request")
    });
    service.map_response(|response| {
        push_trace(response.context, "supergraph_service Rhai map_response")
    });
}
