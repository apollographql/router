### Add `max_reconnect_attempts` and `reconnect_delay` configuration for subscription WebSocket reconnection ([Issue #1440](https://github.com/apollographql/router/issues/1440))

Adds two new fields to the `subscription` configuration block that allow the router to automatically reconnect a dropped WebSocket subscription to a subgraph.

- `max_reconnect_attempts` — how many times to retry a dropped connection (default: `0`, no reconnection)
- `reconnect_delay` — how long to wait before each reconnection attempt (default: `1s`)

```yaml
subscription:
  enabled: true
  max_reconnect_attempts: 5      # retry up to 5 times on connection drop
  reconnect_delay: 2s            # wait 2 seconds between each attempt
  mode:
    passthrough:
      all:
        path: /subscriptions
```

When the WebSocket connection to a subgraph drops and reconnection is configured, the router re-establishes the connection transparently — client subscriptions remain open during the reconnect window and resume receiving events once the connection is restored. After all retry attempts are exhausted the subscription is terminated normally.

Reconnection only applies to WebSocket passthrough mode. Callback-mode subscriptions are unaffected.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/TODO
