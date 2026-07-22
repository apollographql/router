### Fix `connect/v0.4` routing entity resolution through a non-entity `Query` connector (RH-1401)

Under `connect/v0.4`, when a `@key` entity had **both** a type-level reference-resolver `@connect` (mapping from `$this`) **and** a separate `Query` field `@connect` returning that same type (mapping from `$args`, with `entity` unset), connector expansion copied the entity's declared `@key` onto the `Query`-field connector's generated subgraph as **resolvable**. That gave the query planner a second, spurious entity path, so it could resolve entity references through the `Query`-field connector — where `$args` is empty during entity resolution — sending a request with the query parameters stripped and surfacing an upstream error (e.g. a `400` reported as `CONNECTOR_FETCH`).

`connect/v0.3` was never affected, so bumping only the `@link` from `v0.3` to `v0.4` could break a previously working schema. Expansion now only copies keys for `@interfaceObject` types, and always as `resolvable: false`, matching the `v0.3` behavior.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9853
