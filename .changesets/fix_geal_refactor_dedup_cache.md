### Test the in memory cache before deduplicating requests to redis and the query planner ([Issue #2751](https://github.com/apollographql/router/issues/2751))

The cache was built as a serie of layers, from queryd eduplication, to in memory cache then redis cache. Deduplication reduces the amount of in flight requests, but adds some overhead, so we apply the following fixes:
- checking the in memory cache is cheaper than deduplicating the request, so we change the order to check in memory first, then deduplicating calls to redis or the cached service
- futures aware mutexes are slow, so we replace with a parking-lot mutex
- tokio's broadcast channel have some overhead, and we don't exploit them fully (we only broadcast one value), so it is replaced by a read-write lock: one task holds the writer, other tasks wait for the reader to be available

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2853