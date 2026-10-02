### `connect/v0.5` validates that connector responses match the schema's declared shape

When a connector field declares a list return type (e.g. `posts: [Post]`) but the HTTP response is a single object, the router previously nullified the field with no error — making the bug nearly impossible to diagnose. The same was true in reverse (a single-object field receiving an array), and for a non-nullable field (`posts: [Post]!`) whose connector response resolves to `null`, which the router's default configuration only surfaces via the `extensions.valueCompletion` side channel rather than a top-level GraphQL error.

Connectors on `connect/v0.5` now emit an actionable, `CONNECTORS_RESPONSE_SHAPE`-coded error when the response shape doesn't match what the schema declares:
- Schema expects a list, connector returns an object → error
- Schema expects an object, connector returns a list → error
- Schema declares a non-nullable field, connector response resolves to `null` → error

This check is gated to `connect/v0.5` because it changes visible behavior (a case that previously returned `null` now returns an error); `connect/v0.4` and earlier connectors are unaffected when they upgrade the router.

By [@briannafugate408](https://github.com/briannafugate408) in https://github.com/apollographql/router/pull/9714
