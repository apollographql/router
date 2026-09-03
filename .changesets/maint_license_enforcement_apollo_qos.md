### Rate limiting for Free plan accounts now uses a different internal limiter ([PR #9826](https://github.com/apollographql/router/pull/9826))

Routers on the Free plan are rate limited to a fixed number of requests per interval, with excess requests rejected with a `ROUTER_FREE_PLAN_RATE_LIMIT_REACHED` error. The internal implementation of this limiter has changed, so the exact timing of which requests get rejected right at the edge of the limit may be very slightly different. The configured limit and the rejection behavior are otherwise unchanged.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9826