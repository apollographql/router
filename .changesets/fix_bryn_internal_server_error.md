### 5xx Internal server error responses are structured errors ([PR #5159](https://github.com/apollographql/router/pull/5159))

An internal server error (5xx class) which occurs as a result of an unexpected/unrecoverable disruption to the GraphQL request lifecycle execution (e.g., a coprocessor failure, etc.) will now result in a structured GraphQL error (i.e., `{"errors": [...]}`) being returned to the client rather than a plain-text error as was the case previously.

When these circumstances occur, the underling error message — which may be any number of internal disruptions — will still be logged at an `ERROR` level to the router logs for the administrator of the router to monitor and use for debugging purposes.
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5159
