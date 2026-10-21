### Restore structured JSON for `valuable` event fields in JSON logs ([PR #10195](https://github.com/apollographql/router/pull/10195))

Event fields recorded with `tracing::field::valuable` are written as nested JSON objects again by the JSON log formatter, instead of flat `Debug` strings.

A router built with `--cfg tracing_unstable` — for example alongside a native Rust plugin that logs a `#[derive(Valuable)]` struct — used to render such a field as structured JSON:

```json
"log": { "client_id": "abc", "entitlements": { "bypass": true } }
```

Since the JSON formatter's event visitor was replaced to deduplicate the empty `message` emitted by OpenTelemetry macros, the same field rendered as an opaque string, which log tooling can no longer query:

```json
"log": "AccessLog { client_id: \"abc\", entitlements: Entitlements { bypass: true } }"
```

The replacement visitor did not override `record_value`, so it inherited the trait's default implementation, which forwards to the `Debug` rendering. It now converts `valuable` values directly, and the deduplication behavior is unchanged. A value that has no JSON representation, such as a map with non-string keys, still falls back to its `Debug` string, and the reason is recorded under a single `serialization_errors` object keyed by field name (omitted if the event already has its own `serialization_errors` field). The rest of the log line is unaffected.

This applies only to event fields in JSON logs. Span attributes (including the `span` and `spans` entries in JSON logs), the text formatter and exported OpenTelemetry span events still render `valuable` values as `Debug` strings, as they did in 2.15.

Builds that do not set `--cfg tracing_unstable` are unaffected: the supporting crates are declared under that cfg and are neither resolved nor compiled without it.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/10195
