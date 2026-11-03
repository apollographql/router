### Expose persisted-query resolution source on the request context

The router now writes `apollo::supergraph::persisted_query_source` to the request context alongside `apollo::supergraph::persisted_query_id`. The value is `"id"` when the client sent `extensions.persistedQuery.sha256Hash` and the plugin looked up the operation in the manifest, and `"freeform"` when the client sent the full query body and the plugin matched it against the manifest after normalization. The key is absent when no match occurred.

Both paths previously wrote the same `persisted_query_id`, making them indistinguishable from outside the router. The marker that separated them (`UsedQueryIdFromManifest`) was a Rust-only extension type, unreachable from YAML selectors, Rhai, or coprocessor responses. Downstream telemetry can now tag the router span with the source via one YAML line:

```yaml
spans:
  router:
    attributes:
      persisted_query.source:
        response_context: "apollo::supergraph::persisted_query_source"
        default: "none"
```

This closes an observability gap for teams running staged migrations toward safelisting — before this, "my client is in the manifest and all its requests are matched" and "my client has migrated to the hash wire format" looked identical in gateway telemetry, which made the final transition to `safelist.require_id: true` a leap of faith.

By [@kwahome](https://github.com/kwahome) in https://github.com/apollographql/router/pull/<PR_NUMBER>
