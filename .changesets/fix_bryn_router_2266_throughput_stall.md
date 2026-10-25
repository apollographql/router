### Throughput no longer levels off before the router runs out of CPU

Under sustained heavy load, a 3.x router could stop taking on more requests well before it used its CPU. Throughput levelled off at around 1,800–2,000 requests per second on 4 cores in our tests, while latency climbed to 150–250 ms and up to a core sat idle. The router now keeps scaling until its CPU is busy.

This applies when neither `traffic_shaping.router.global_rate_limit` nor `traffic_shaping.router.concurrency_limit` is set. With either one set, requests still pass through that limit one at a time.

A saturated router still answers `503 Service Unavailable` with the `REQUEST_CONCURRENCY_LIMITED` error code. It now does so once 50,000 requests are in progress.

By [@BrynCooke](https://github.com/BrynCooke)
