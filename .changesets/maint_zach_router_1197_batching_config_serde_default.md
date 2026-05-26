### Refactor batching configuration struct to follow YAML design guidance

The `batching` configuration struct now uses a struct-level `#[serde(default)]` with an explicit `impl Default`, rather than per-field `#[serde(default)]` annotations. This aligns with the project's [YAML design guidance](https://github.com/apollographql/router/blob/dev/dev-docs/yaml-design-guidance.md#use-serdedefault-on-struct-instead-of-fields-when-possible), which requires that the serde deserialization path and the `Default` implementation use the same mechanism.

As a result of this change:

- The `mode` field now has an explicit default of `batch_http_link` (the only supported mode), so it is no longer required in YAML configuration. Existing configurations that specify `mode: batch_http_link` are unaffected.
- The `subgraph` batching configuration now rejects unknown fields, consistent with the rest of the router configuration.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9315
