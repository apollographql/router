// Edits response data, then throws when the chunk asks for it.

fn supergraph_service(service) {
    service.map_response(|response| {
        response.body.data.edited = true;
        if response.body.data.fail == true {
            throw "response failure";
        }
    });
}
