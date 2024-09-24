fn supergraph_service(service) {
    const response_callback = Fn("process_response");
    service.map_response(response_callback);
}

fn process_response(response) {
    const estimate = response.context[Router.APOLLO_COST_ESTIMATED_KEY];
    response.headers["demand-control-estimate"] = to_string(estimate);

    const actual = response.context[Router.APOLLO_COST_ACTUAL_KEY];
    response.headers["demand-control-actual"] = to_string(actual);

    const strategy = response.context[Router.APOLLO_COST_STRATEGY_KEY];
    response.headers["demand-control-strategy"] = strategy;

    const result = response.context[Router.APOLLO_COST_RESULT_KEY];
    response.headers["demand-control-result"] = result;
}