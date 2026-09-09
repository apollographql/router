### Restore structured JSON logging for `valuable` fields ([PR #10195](https://github.com/apollographql/router/pull/10195))

Fields recorded with `tracing::field::valuable` are serialized as nested JSON objects again, instead of flat `Debug` strings.

A router built with `--cfg tracing_unstable` — for example alongside a native Rust plugin that logs a `#[derive(Valuable)]` struct — used to render such a field as structured JSON:

```json
"log": { "client_id": "abc", "entitlements": { "bypass": true } }
```

Since the JSON formatter's event visitor was replaced to deduplicate the empty `message` emitted by OpenTelemetry macros, the same field rendered as an opaque string, which log tooling can no longer query:

```json
"log": "AccessLog { client_id: \"abc\", entitlements: Entitlements { bypass: Some(true) } }"
```

The replacement visitor did not override `record_value`, so it inherited the trait's default implementation, which forwards to the `Debug` rendering. It now converts `valuable` values directly, and the deduplication behavior is unchanged.

Builds that do not set `--cfg tracing_unstable` are unaffected: the supporting crates are declared under that cfg and are neither resolved nor compiled without it.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/10195
