### Fix SigV4 signing params leaking across subgraphs in the same operation ([PR #9385](https://github.com/apollographql/router/pull/9385))

When `authentication.subgraph.subgraphs` was configured with `aws_sig_v4` for a specific subgraph, the signing parameters were stored in the shared operation `Context`. Because all subgraph requests in the same operation share the same `Context`, the SigV4 params were visible to every subgraph, causing unconfigured subgraphs to have their `Authorization` header overwritten with AWS credentials.

Signing parameters are now stored in the individual HTTP request's extensions, which are scoped to a single subgraph request and not shared across the operation.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9385
