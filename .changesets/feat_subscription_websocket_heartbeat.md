### Subscriptions: Add configurable "heartbeat" to subgraph WebSocket protocol ([Issue #4621](https://github.com/apollographql/router/issues/4621))

To account for GraphQL Subscription WebSocket implementations (e.g., [DGS](https://netflix.github.io/dgs/)) which drop idle connections by design, the router adds the ability to configure a heartbeat to keep active connections alive.

An example of configuration:

```yaml
subscription:
  mode:
    passthrough:
      all:
        path: /graphql
        heartbeat_interval: enable #Optional
 ```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/4802
