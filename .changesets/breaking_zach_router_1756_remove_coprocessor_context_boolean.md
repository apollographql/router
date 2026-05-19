### Remove deprecated boolean form of `coprocessor.<stage>.context`

The boolean form of the `context` field on each coprocessor stage (`context: true` / `context: false`) was deprecated and has now been removed across all 10 coprocessor stage configurations (`router.{request,response}`, `supergraph.{request,response}`, `execution.{request,response}`, `subgraph.all.{request,response}`, `connector.all.{request,response}`). Use the structured form instead:

- `context: all` — send every context entry with the current key names
- `context: deprecated` — send every context entry using the pre-2.x deprecated key names
- `context: selective: [key1, key2, ...]` — only send the listed context keys
- `context: none` — do not send context (the default; equivalent to omitting the field)

To preserve existing behavior, an automatic configuration migration rewrites `context: true` to `context: deprecated` and `context: false` to `context: none` on router startup. See [https://go.apollo.dev/o/coprocessor-context](https://go.apollo.dev/o/coprocessor-context) for details.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9453
