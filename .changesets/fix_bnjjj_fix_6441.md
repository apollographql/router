### Entity cache: fix directive conflicts in cache-control header ([Issue #6441](https://github.com/apollographql/router/issues/6441))

Unnecessary cache-control directives are created in cache-control header.  The router will now filter out unnecessary values from the `cache-control` header when the request resolves. So if there's `max-age=10, no-cache, must-revalidate, no-store`, the expected value for the cache-control header would simply be `no-store`. Please see the MDN docs for justification of this reasoning: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#preventing_storing

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6543