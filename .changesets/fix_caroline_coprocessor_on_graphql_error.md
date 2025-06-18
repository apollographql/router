### Fix `on_graphql_error` selector ([PR #7669](https://github.com/apollographql/router/pull/7669))

The `on_graphql_error` selector will now correctly fire on the supergraph stage; previously it only worked on the router stage.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7669