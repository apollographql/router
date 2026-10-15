### Send coprocessors context keys specific to the current subgraph request ([PR #10501](https://github.com/apollographql/router/pull/10501))

In subgraph coprocessor stages, a `selective` context entry can now be an object with `append_subgraph_request_id: true`. The router sends the context key named `name` followed by the current subgraph request's ID:

```yaml
coprocessor:
  subgraph:
    all:
      response:
        context:
          selective:
            - apollo::supergraph::operation_name
            - name: "my_coprocessor::state::"
              append_subgraph_request_id: true
```

The context is shared by every subgraph request made for a client request, and subgraph requests can run in parallel, so a fixed context key can't carry data from a `SubgraphRequest` stage to its matching `SubgraphResponse` stage. With this option, the `SubgraphRequest` stage can store data under a key that includes its subgraph request ID, and only the matching `SubgraphResponse` stage receives it. The router rejects `append_subgraph_request_id` outside subgraph stages.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/10501
