### Client name support for Persisted Query Lists ([PR #6198](https://github.com/apollographql/router/pull/6198))

The persisted query manifest fetched from Uplink can now contain a `clientName` field in each operation. Two operations with the same `id` but different `clientName` are considered to be distinct operations (and may have distinct bodies).

Router resolves the client name by taking the first of these which exists:
- Reading the `apollo_persisted_queries::client_name` context key (which may be set by a `router_service` plugin)
- Reading the HTTP header named by `telemetry.apollo.client_name_header` (which defaults to `apollographql-client-name`)

If a client name can be resolved for a request, Router first tries to find a persisted query with the specified ID and the resolved client name.

If there is no operation with that ID and client name, or if a client name cannot be resolved, Router tries to find a persisted query with the specified ID and no client name specified.  (This means that existing PQ lists that do not contain client names will continue to work.)

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/6198