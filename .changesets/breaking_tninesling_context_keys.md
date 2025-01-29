### Update context key names for consistency and document them ([PR #6572](https://github.com/apollographql/router/pull/6572))

Documentation and naming refactor for context keys. The new unified taxonomy for context keys is formatted at `apollo::` + router stage or plugin name + `::` + key descriptor.

Selected examples:
`operation_name` -> `apollo::supergraph::operation_name`
`apollo_authentication::JWT::claims` -> `apollo::authentication::jwt_claims`

A full compendium of available context keys in this new format can be found in the request lifecycle section of [docs/source/routing/customization/overview.mdx](https://github.com/apollographql/router/blob/tninesling/context-docs/docs/source/routing/customization/overview.mdx#request-context).

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6572
