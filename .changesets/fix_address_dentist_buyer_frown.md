### Fix coprocessor empty body object panic ([PR #6398](https://github.com/apollographql/router/pull/6398))

Previously, the router would panic if a coprocessor responds with an empty body object at the supergraph stage: 

```json
{
  ... // other fields
  "body": {} // empty object
}
```

This has been fixed in this release.

> Note: the previous issue didn't affect coprocessors that responded with formed responses.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6398
