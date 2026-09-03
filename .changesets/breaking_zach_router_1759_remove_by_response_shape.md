### Remove deprecated `actual_cost_mode: by_response_shape` demand control mode

The `by_response_shape` variant of `demand_control.strategy.static_estimated.actual_cost_mode` was deprecated (with a startup warning) and has now been removed. `by_response_shape` computed cost from only the final shape of the composed response, which under-counted the work done by federated lookups whose results did not make it into the client response.

If your router config explicitly sets `actual_cost_mode: by_response_shape`, change it to `actual_cost_mode: by_subgraph` (which is also the default, so the field can be removed entirely). `by_subgraph` sums the cost of each subgraph response and more closely mirrors the cost estimation strategy.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9555
