### Fix cost calculation for subgraph requests with named fragments ([PR #6162](https://github.com/apollographql/router/issues/6162))

In some cases where subgraph GraphQL operations contain named fragments and abstract types, demand control used the wrong type for cost calculation, and could reject valid operations.
Now, the correct type is used.

This fixes errors of the form:
```
Attempted to look up a field on type MyInterface, but the field does not exist
```

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6162