### Add coprocessor metrics ([PR #3483](https://github.com/apollographql/router/pull/3483))

Introduces a new metric for the router:

```
apollo.router.operations.coprocessor
```

It has two attributes:

```
coprocessor.stage: string (RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse)
coprocessor.succeeded: bool
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3483