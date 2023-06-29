### Update example for claim forwarding ([Issue #3224](https://github.com/apollographql/router/issues/3224))

The JWT claim example that we had in our docs was insecure as it iterated over the list of claims and set them as headers.
A malicious user could have provided a valid JWT that was missing claims and then set those claims as headers.
This would only have affected users who had configured their routers to forward all headers from the client to subgraphs.

The documentation has been updated to explicitly list the claims that are forwarded to the subgraph.
In addition, a new example has been added that uses extensions to forward claims.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3319
