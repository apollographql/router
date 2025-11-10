### Ability to set persisted query operation ID via plugin ([PR #7771](https://github.com/apollographql/router/pull/7771))


A `router_service` plugin can determine the persisted query operation ID for the request and add it to the request context under the key `apollo_persisted_queries::operation_id`. If this is set, the Router will not look in the `persistedQuery` request extension for an operation ID.

This lets you use custom clients that put the operation ID in a header, path name, or query parameter instead. This is especially helpful for onboarding existing clients from other PQ systems, and for path-based logging.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/7771