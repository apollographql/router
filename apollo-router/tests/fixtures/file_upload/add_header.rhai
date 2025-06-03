fn process_subgraph_request(request, subgraph) {
  request.subgraph.headers["x-bla-bla"] = "bla";
}

fn subgraph_service(service, subgraph) {
  const request_callback = |request| {
    process_subgraph_request(request, subgraph);
  };
  service.map_request(request_callback);
}
