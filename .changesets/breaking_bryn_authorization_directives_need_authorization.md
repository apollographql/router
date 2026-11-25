### Authorization directives need the Authorization license feature ([PR #10798](https://github.com/apollographql/router/pull/10798))

If your supergraph uses `@authenticated`, `@requiresScopes` or `@policy`, the router needs a license that includes the Authorization feature. Without one, the router won't start, and a reload with that supergraph is refused while the router keeps serving its current one.

To run without the Authorization feature, set `authorization.directives.enabled: false` in router.yaml. The router then doesn't enforce these directives, so it serves the fields they protect without checking them.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10798
