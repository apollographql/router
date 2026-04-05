### Add metrics for traffic_shaping ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Adds the following metrics for traffic_shaping activity:
- apollo.router.operations.traffic_shaping.timeout
- apollo.router.operations.traffic_shaping.load_shed
Both counters include a subgraph.service.name attribute so consumers of the metric can break down error rates by subgraph.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8905
