### Authorization directives are analysed once per operation ([PR #10798](https://github.com/apollographql/router/pull/10798))

When a supergraph uses `@authenticated`, `@requiresScopes` or `@policy`, the router analysed every request's operation for these directives and read the supergraph's `@link` definitions several times to do it. It now reads the definitions once when the schema loads and analyses an operation the first time it is parsed, reusing the result for later requests with the same query. On the largest supergraph measured, on a Router 3.x pre-release build, this analysis cost about 300 µs of CPU per request, 213 µs of it spent reading `@link` definitions; a repeated operation now skips both.

You don't need to change any configuration.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10798
