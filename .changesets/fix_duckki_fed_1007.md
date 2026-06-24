### Restore defer dependencies that were lost by query plan reduction ([PR #9443](https://github.com/apollographql/router/pull/9443))

This fixes a query planner bug where the deferred block of an `@defer` query could be missing field values that should have been forwarded from the primary block, resulting in `null` fields or absent data in the deferred chunk at runtime.

When the query planner builds the fetch dependency graph, it runs a reduction step that prunes redundant "must run before" edges. That step could drop edges whose source fetch was the only producer of fields the deferred block needed (typically `__typename` or entity keys). The planner now detects those dropped edges and restores them as deferred dependencies so the deferred block receives the values it needs.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9443
