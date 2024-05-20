### 5xx internal server error responses returned as GraphQL structured errors ([PR #5159](https://github.com/apollographql/router/pull/5159))

Previously, the router returned internal server errors (5xx class) as plaintext to clients. Now in this release, the router returns these 5xx errors as structured GraphQL (for example, `{"errors": [...]}`). 

Internal server errors are returned upon unexpected or unrecoverable disruptions to the GraphQL request lifecycle execution. When these occur, the underlying error messages are logged at an `ERROR` level to the router's logs.
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5159
