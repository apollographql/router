### Authorization directives need the Authorization license feature ([PR #10798](https://github.com/apollographql/router/pull/10798))

A supergraph that uses `@authenticated`, `@requiresScopes` or `@policy` now needs a license that includes the Authorization feature. Before, the router accepted `@authenticated` and `@requiresScopes` on any license that included Authentication, and accepted `@policy` with any license or none.

If your router serves these directives without a license that includes Authorization, it refuses the schema after you upgrade: it won't start, and a reload with that schema is refused while the router keeps serving its current one. This applies whether or not router.yaml has an `authorization:` section, because the directives are enforced by default. The error lists `authorization.directives.enabled` under Authorization, including when router.yaml doesn't set it.

To migrate, get a license that includes Authorization, or remove the directives from your subgraph schemas and recompose. Setting `authorization.directives.enabled: false` also lets the router start, but it then serves the fields these directives protect without checking them.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10798
