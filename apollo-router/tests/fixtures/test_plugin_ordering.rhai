fn router_http(service) {
    service.map_request(|request| {
        request.context.upsert("ordering-trace", |v| {
            let a = if v.type_of() == "array" { v } else { [] };
            a.push("router_http Rhai map_request");
            a
        })
    });
    service.map_response(|response| {
        response.context.upsert("ordering-trace", |v| {
            let a = if v.type_of() == "array" { v } else { [] };
            a.push("router_http Rhai map_response");
            a
        })
    });
}

fn supergraph_service(service) {
    service.map_request(|request| {
        request.context.upsert("ordering-trace", |v| {
            let a = if v.type_of() == "array" { v } else { [] };
            a.push("supergraph_service Rhai map_request");
            a
        })
    });
    service.map_response(|response| {
        response.context.upsert("ordering-trace", |v| {
            let a = if v.type_of() == "array" { v } else { [] };
            a.push("supergraph_service Rhai map_response");
            a
        })
    });
}
