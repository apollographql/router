// Supergraph response callbacks for the bridge performance measurement. The workload is chosen
// by the `workload` context entry, so every workload runs the same callback.

fn supergraph_service(service) {
    service.map_response(|response| {
        let workload = response.context["workload"];
        if workload == "read_only" {
            response.context["name"] = response.body.data.users[0].name;
        } else if workload == "sparse" {
            response.body.data.users[0].name = "edited";
        } else if workload == "dense" {
            let data = response.body.data;
            for i in 0..data.users.len() {
                data.users[i].score += 1.0;
                data.users[i].tags.push("seen");
            }
            response.body.data = data;
        }
    });
}
