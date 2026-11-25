### Authorization directives are analysed once per operation ([PR #10798](https://github.com/apollographql/router/pull/10798))

When a supergraph uses `@authenticated`, `@requiresScopes` or `@policy`, the router analysed every request's operation for these directives and read the supergraph's `@link` definitions several times to do it. On the largest supergraph measured, on a Router 3.x pre-release build, this cost about 300 µs of CPU per request, 213 µs of it spent reading `@link` definitions.

The router now reads the definitions once, when the schema loads, and analyses each operation only the first time it is parsed, reusing the result for later requests with the same query. You don't need to change any configuration.

Routers that use these directives without an `authorization:` section in router.yaml now also report the `apollo.router.operations.authorization` metric, as routers with the section already did.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10798
