### Reduce Redis maintenance worker backpressure with batch-drain and deduplication ([PR #9642](https://github.com/apollographql/router/pull/9642))

The response cache uses Redis ZSETs as invalidation indexes — each cache entry is a member scored by its expiry timestamp. A background maintenance worker periodically calls `ZREMRANGEBYSCORE` to purge expired members. Under heavy write load, the worker's channel could accumulate thousands of identical keys, causing it to issue redundant Redis commands and fall behind.

This fix changes the worker to batch-drain up to 1,000 pending keys per cycle and deduplicate them into a `HashSet` before issuing any Redis commands, ensuring at most one `ZREMRANGEBYSCORE` call per unique key per cycle regardless of how many duplicates were queued.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9642
