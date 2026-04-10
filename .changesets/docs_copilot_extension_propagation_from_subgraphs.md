### Document how to propagate extensions from subgraph responses

Added comprehensive documentation explaining how to propagate the root-level `extensions` field from subgraph responses to the final client response using Rhai scripts.

The documentation includes:

- **Why extensions aren't propagated by default**: Explains the potential for key conflicts when merging extensions from multiple subgraphs
- **Complete working example**: Shows how to safely collect extensions from subgraph responses using the request context and merge them in the supergraph service
- **Important considerations**: Highlights race conditions, key conflicts, and the non-deterministic ordering of parallel subgraph requests
- **Customization options**: Provides an advanced example showing how to implement custom conflict resolution logic

This addresses the common question about why subgraph extensions don't appear in client responses and provides users with a clear path to implement extension propagation when needed.

By [@copilot](https://github.com/copilot) in https://github.com/apollographql/router/pull/TBD
