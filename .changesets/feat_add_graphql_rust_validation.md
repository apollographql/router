## Experimental support for GraphQL validation in Rust

We are experimenting with a new GraphQL validation implementation written in Rust. The legacy implementation is part of the JavaScript query planner. This is part of a project to remove JavaScript from the Router to improve performance and memory behavior.

To opt in to the new validation implementation, set:

```yaml {4,8} title="router.yaml"
experimental_graphql_validation_mode: new
```

Or use `both` to run the implementations side by side and log a warning if there is a difference in results:

```yaml {4,8} title="router.yaml"
experimental_graphql_validation_mode: both
```

This is an experimental option while we are still finding edge cases in the new implementation, but it will become the default in the future.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/3134
