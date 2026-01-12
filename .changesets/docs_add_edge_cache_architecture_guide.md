### Add multi-region edge cache architecture guide ([PR #8803](https://github.com/apollographql/router/pull/8803))

Added a new documentation page describing how to deploy GraphOS Router with Redis as part of a globally distributed edge caching system. The guide covers:

- Multi-tier caching architecture (L1 in-process, L2 regional Redis, L3 optional global store)
- Multi-region router deployment with regional Redis instances
- Event-driven cache invalidation using pub/sub patterns
- CDN and edge layer integration
- Subgraph-level caching strategies
- Implementation checklist for production deployments

This reference architecture helps users understand how to build a complete edge caching solution around the router's response caching feature.

By [@the-gigi-apollo](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8803
