### Use BoxCloneService instead of BoxService in router pipeline services ([PR #9161](https://github.com/apollographql/router/pull/9161))

All service hooks in the `Plugin` trait (and `PluginUnstable`) now receive and return `BoxCloneService` instead of `BoxService`.

If you have a native Rust plugin that implements any of the service hooks, update the signatures to use `BoxCloneService`:

| Hook | Before | After |
|---|---|---|
| `router_service` | `fn router_service(&self, service: router::BoxService) -> router::BoxService` | `fn router_service(&self, service: router::BoxCloneService) -> router::BoxCloneService` |
| `supergraph_service` | `fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService` | `fn supergraph_service(&self, service: supergraph::BoxCloneService) -> supergraph::BoxCloneService` |
| `execution_service` | `fn execution_service(&self, service: execution::BoxService) -> execution::BoxService` | `fn execution_service(&self, service: execution::BoxCloneService) -> execution::BoxCloneService` |
| `subgraph_service` | `fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService` | `fn subgraph_service(&self, name: &str, service: subgraph::BoxCloneService) -> subgraph::BoxCloneService` |

Any custom middleware layers you apply inside these hooks must now produce a `Clone`-able service. In practice this means your `tower::Layer` implementations should wrap services that implement `Clone` (or use `BoxCloneService::new` when boxing).

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9161
