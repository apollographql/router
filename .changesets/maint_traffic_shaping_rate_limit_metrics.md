### Report configured traffic-shaping rate limits in router config metrics ([Issue #ROUTER-1664](https://apollographql.atlassian.net/browse/ROUTER-1664))

The `apollo.router.config.traffic_shaping` metric gains four attributes: `opt.router.rate_limit.capacity` and `opt.router.rate_limit.interval` for the router-level rate limit, and `opt.subgraph.rate_limit.capacity` and `opt.subgraph.rate_limit.interval` for the rate limit applied to all subgraphs. Each attribute carries the configured value directly, so you can confirm what a running router enforces without reading its YAML file. Per-subgraph rate limit overrides still only appear as the existing `opt.subgraph.rate_limit` boolean, because attributing them by subgraph name would grow the metric's cardinality with every subgraph added.

By [@bryncooke](https://github.com/bryncooke)
