### Implement streaming compression for subgraph requests ([Issue #4648](https://github.com/apollographql/router/issues/4648))

This fixe subgraph HTTP requests to compress the body in streaming instead of loading it entirely in the compression engine before sending everything at once. This reuses the compression layer that the router uses to compress client responses.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4672