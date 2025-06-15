### Improved content type handling for subgraph communications ([PR #7691](https://github.com/apollographql/router/pull/7691))

The router now handles HTTP content types more consistently when communicating with federated subgraphs. This improvement consolidates content type validation logic and ensures proper handling of both `application/json` and `application/graphql-response+json` responses from subgraphs.

This change maintains full backward compatibility while improving the internal organization of content type handling code.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7691
