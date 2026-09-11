### Query plan warm-up no longer logs a false "Authorization error" ([PR #10210](https://github.com/apollographql/router/pull/10210))

When query plan warm-up loads persisted operations, it plans them without auth context. Authorization filtering can remove every field from a `@policy`-protected operation, causing warm-up to log an ERROR-level "Authorization error" during startup or reload.

Warm-up now suppresses this log event. Client requests still log the event when authorization filtering rejects the entire operation on a query plan cache miss and authorization error logging is enabled. Cache hits, including cached rejections populated by warm-up, bypass this log.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10210
