### Fix spurious `REQUEST_RATE_LIMITED` errors when no rate limiting is configured ([PR #9034](https://github.com/apollographql/router/pull/9034))

Under sustained load, the router could return `REQUEST_RATE_LIMITED` (429) errors even when no rate limiting was configured. The `Buffer` worker processes requests in a tight loop, and because `LoadShed` never yields, the worker could exhaust Tokio's cooperative scheduling budget in one cycle. Once exhausted, the semaphore inside the inner `Buffer` would report itself as not ready — not because it was full, but because the coop budget check ran before the semaphore was even consulted. `LoadShed` interpreted this as backpressure and shed the request.

This fix introduces `UnconstrainedBuffer`, which wraps `Buffer::poll_ready` in `tokio::task::unconstrained` so that semaphore availability is checked independently of the coop budget. Requests are now shed only when the buffer is genuinely full.

By [@jhrldev](https://github.com/jhrldev) in https://github.com/apollographql/router/pull/9034
