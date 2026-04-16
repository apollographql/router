### Fix spurious `REQUEST_RATE_LIMITED` errors when no rate limiting is configured ([PR #9034](https://github.com/apollographql/router/pull/9034))

Under sustained load, the router could return `REQUEST_RATE_LIMITED` (429) errors even when no rate limiting was configured. An internal queue had an implicit limit that could trigger load shedding, even if the queue was not _actually_ overloaded.

This fix removes that implicit limit, so requests are shed only when the queue is genuinely full. The queue still has explicit limits to ensure quality of service.

By [@jhrldev](https://github.com/jhrldev) in https://github.com/apollographql/router/pull/9034
