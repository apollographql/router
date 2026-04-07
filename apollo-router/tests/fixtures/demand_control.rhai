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

    const estimates_by_subgraph = response.context[Router.APOLLO_COST_BY_SUBGRAPH_ESTIMATED_KEY];
    response.headers["demand-control-estimate-subgraph-products"] = to_string(estimates_by_subgraph["products"]);
    response.headers["demand-control-estimate-subgraph-users"] = to_string(estimates_by_subgraph["users"]);
    response.headers["demand-control-estimate-subgraphs"] = to_string(estimates_by_subgraph.len());

    const actuals_by_subgraph = response.context[Router.APOLLO_COST_BY_SUBGRAPH_ACTUAL_KEY];
    response.headers["demand-control-actual-subgraph-products"] = to_string(actuals_by_subgraph["products"]);
    response.headers["demand-control-actual-subgraph-users"] = to_string(actuals_by_subgraph["users"]);
    response.headers["demand-control-actual-subgraphs"] = to_string(actuals_by_subgraph.len());

    const results_by_subgraph = response.context[Router.APOLLO_COST_BY_SUBGRAPH_RESULT_KEY];
    response.headers["demand-control-result-subgraph-products"] = to_string(results_by_subgraph["products"]);
    response.headers["demand-control-result-subgraph-users"] = to_string(results_by_subgraph["users"]);
    response.headers["demand-control-result-subgraphs"] = to_string(actuals_by_subgraph.len());
}