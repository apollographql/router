// Two callbacks per direction; each records whether it saw the other one's edit.

fn supergraph_service(service) {
    service.map_request(|request| {
        request.body.variables.first_saw_second = request.body.variables.second_ran;
        request.body.variables.first_ran = true;
    });
    service.map_request(|request| {
        request.body.variables.second_saw_first = request.body.variables.first_ran;
        request.body.variables.second_ran = true;
    });
    service.map_response(|response| {
        response.body.data.first_saw_second = response.body.data.second_ran;
        response.body.data.first_ran = true;
    });
    service.map_response(|response| {
        response.body.data.second_saw_first = response.body.data.first_ran;
        response.body.data.second_ran = true;
    });
}
