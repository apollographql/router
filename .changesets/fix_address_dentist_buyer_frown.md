### Fix coprocessor empty body object panic ([PR #6398](https://github.com/apollographql/router/pull/6398))
If a coprocessor responds with an empty body object at the supergraph stage then the router would panic.

```json
{
  ... // other fields
  "body": {} // empty object
}
```

This does not affect coprocessors that respond with formed responses.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6398
