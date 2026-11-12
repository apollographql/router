### Router uses all of its CPU under heavy load

Under sustained heavy load, a 3.x router could stop taking on more requests before its CPU was busy, so latency climbed while cores sat idle. The router now keeps taking requests until its CPU is busy.

This applies when neither `traffic_shaping.router.global_rate_limit` nor `traffic_shaping.router.concurrency_limit` is set. Those limits still apply across all requests.

An overloaded router still answers `503 Service Unavailable` with the `REQUEST_CONCURRENCY_LIMITED` error code, now once 50,000 requests are in progress at once on a listen address.

By [@BrynCooke](https://github.com/BrynCooke)
