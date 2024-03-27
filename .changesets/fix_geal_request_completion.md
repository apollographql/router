### Execute the entire request pipeline if the client closed the connection ([Issue #4569](https://github.com/apollographql/router/issues/4569)), [Issue #4576](https://github.com/apollographql/router/issues/4576)), ([Issue #4589](https://github.com/apollographql/router/issues/4589)), ([Issue #4590](https://github.com/apollographql/router/issues/4590)), ([Issue #4611](https://github.com/apollographql/router/issues/4611))

The router is now making sure that the entire request handling pipeline is executed when the client closes the connection early, to let telemetry and any rhai scrit or coprocessor perform their tasks before canceling. Before that, when a client canceled a request, the entire execution was dropped and parts of the router, like telemetry, could not run properly. It now executes up to the first response event (in the case of subscription or `@defer` usage), adds a 499 status code to the response and skips the remaining subgraph requests.

This change will report more requests to Studio and the configured telemetry, which will appear like a sudden increase in errors, because those failing requests were not reported before. To keep the previous behavior of immediately dropping execution for canceled requests, it is possible with the following option:

```yaml
supergraph:
  early_cancel: true
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4770