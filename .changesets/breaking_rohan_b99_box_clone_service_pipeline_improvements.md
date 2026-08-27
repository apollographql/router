### BoxCloneService router pipeline improvements ([PR #9169](https://github.com/apollographql/router/pull/9169))

The `ServiceFactory`, `MakeSubgraphService` and `MakeHttpService` traits have been removed from router internals, as well as unnecessary `buffered` calls in the execution, supergraph, connector, and file-upload service pipelines.

Previously, each of these pipelines wrapped their inner service in a `Buffer` layer, which absorbed `poll_ready` `Pending` signals from the inner service. With those buffers removed, `Pending` now propagates directly through to the outer router buffer's worker. Under high load, operators may observe different request-acceptance behaviour (e.g. earlier back-pressure) compared to prior releases.

**Breaking Changes**
- `apollo_router::services::router::BoxService` has been removed. Use `apollo_router::services::router::BoxCloneService` instead.
- `apollo_router::services::subgraph::BoxService` has been removed. Use `apollo_router::services::subgraph::BoxCloneService` instead.
- `apollo_router::services::supergraph::BoxService` has been removed. Use `apollo_router::services::supergraph::BoxCloneService` instead.
- `apollo_router::services::execution::BoxService` has been removed. Use `apollo_router::services::execution::BoxCloneService` instead.
- Removed intermediate `Buffer` layers from the execution, supergraph, connector, and file-upload service pipelines. `poll_ready` back-pressure from inner services now propagates directly to the outer router buffer instead of being absorbed.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9169
