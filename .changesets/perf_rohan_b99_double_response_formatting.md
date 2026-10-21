### Format each response once instead of twice ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

The router formatted almost every response twice, even with authorization turned off. The second pass is only needed when authorization (`@authenticated`, `@requiresScopes` or `@policy`) removes fields from an operation, and the router now runs it only then. This lowers CPU use per response, most of all for large responses, and cached query plans take less memory.

Responses are unchanged.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
