### Fix query planning ignoring queries modified by coprocessors/plugins

Query analysis parses the incoming query and stores the resulting document in the request context *before* coprocessors or Rhai/native plugins run. If a plugin rewrote the query string afterwards, query planning previously kept using that stale, pre-modification document, so the rewritten query had no effect on planning.

`CachingQueryPlanner` now detects when the query string no longer matches the document it has in context and re-parses it, so query planning always operates on the actual (possibly modified) query.
