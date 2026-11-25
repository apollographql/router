### Settings your license doesn't cover stop the router from starting ([PR #10856](https://github.com/apollographql/router/pull/10856), [PR #10798](https://github.com/apollographql/router/pull/10798))

Some licensed settings used to be ignored when your license didn't include their feature: the router started, logged a warning and ran without them. A router configured with `authorization.require_authentication: true`, for example, accepted unauthenticated requests. The router now refuses to start or reload with these settings, as it already did for other licensed settings, and the error names each one. A refused reload leaves the router serving its current configuration and supergraph.

If your license doesn't include the feature, these settings now stop startup:

| Setting | License feature |
|---|---|
| `authorization.require_authentication: true` | Authorization |
| A supergraph that uses `@authenticated`, `@requiresScopes` or `@policy`, unless `authorization.directives.enabled: false` | Authorization |
| `authentication.subgraph` or `authentication.connector` | Authentication |
| A `subscription:` section without `enabled: false` | Subscriptions |

The authorization directives need the Authorization feature whether or not router.yaml has an `authorization:` section. With `authorization.directives.enabled: false`, the router doesn't enforce them, so it serves the fields they protect without checking them.

These sections no longer stop startup, because they turn nothing on:

- `batching` and `persisted_queries` sections that don't set `enabled: true`
- a `demand_control` section with `enabled: false`
- an `authorization` section without `require_authentication: true`, when the supergraph doesn't use the authorization directives

To migrate, remove each setting the error names, set `enabled: false` where the table says so, or get a license that includes the feature.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10856 and https://github.com/apollographql/router/pull/10798
