### Scope SigV4 signing params to a single subgraph request instead of sharing them across the operation ([PR #9385](https://github.com/apollographql/router/pull/9385))

When `authentication.subgraph.subgraphs` configured `aws_sig_v4` for a specific subgraph, the signing parameters were visible to every other subgraph in the same operation — causing unconfigured subgraphs to have their `Authorization` header overwritten with AWS credentials.

Signing parameters are now scoped to the individual subgraph HTTP request rather than the operation, so AWS credentials only travel with requests to the subgraph they were configured for.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9385
