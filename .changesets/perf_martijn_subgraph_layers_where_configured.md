### Subgraph requests pass only the layers their subgraph's configuration uses

Every subgraph request passed the subscription layer, the subgraph APQ layer, the subgraph batching layer and traffic shaping's admission buffer, whether or not the router configured subscriptions, APQ, batching or traffic shaping for that subgraph. Each of these copied the rest of the stack, boxed a future or handed the request to a worker task on every request. Each layer is now placed only where its configuration applies to the subgraph:

- the subscription layer where the `subscription` plugin is enabled with a mode for the subgraph;
- the APQ layer where `apq.subgraph` enables APQ for the subgraph;
- the batching layer where `batching` is enabled and its `subgraph` settings include the subgraph;
- traffic shaping's layers where `traffic_shaping` has a block for the subgraph or `all`, an empty one included.

The header rules and the subscription layer no longer copy the stack beneath them for each request. Answers, metrics and configuration are unchanged.

By [@martijnwalraven](https://github.com/martijnwalraven)
