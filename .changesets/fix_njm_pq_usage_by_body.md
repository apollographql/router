### Report PQ usage when an operation is requested by safelisted operation body ([PR #8168](https://github.com/apollographql/router/pull/8168))

Previously we would only record PQ metrics for operations that were requested by the PQ ID. This change updates usage reporting so that we also report usage if the PQ operation is requested by the safelisted operation body.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8168