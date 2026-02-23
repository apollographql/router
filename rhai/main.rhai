
fn subgraph_service(service, subgraph) {
  let request_callback = |req| {
      let auth = req.headers["auth"].trim();
      print(`Subgraph service: Ready to send sub-operation to subgraph ${subgraph}`);
  };

  let response_callback = |response| {
      print(`Subgraph service: Received sub-operation response from subgraph ${subgraph}`);
  };
  service.map_request(request_callback);
  service.map_response(response_callback);
}