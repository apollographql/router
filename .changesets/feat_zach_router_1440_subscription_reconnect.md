### Add `max_reconnect_attempts` and `reconnect_delay` configuration for subscription WebSocket reconnection

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

When the WebSocket connection to a subgraph drops and reconnection is configured, the router re-establishes the connection transparently — client subscriptions remain open during the reconnect window and resume receiving events once the connection is restored. After all retry attempts are exhausted the router forwards the final transport error to the client and terminates the subscription.

Reconnection only applies to WebSocket passthrough mode. Callback-mode subscriptions are unaffected.

Two behaviors worth noting when enabling reconnection:

- `max_reconnect_attempts` is a per-disconnect budget, not a lifetime total: the budget refreshes after a connection stays stable and then drops, so a long-lived subscription may reconnect more than `max_reconnect_attempts` times overall. Use `max_lifetime` for a hard ceiling.
- Reconnection reuses the `connectionParams` (including any propagated `Authorization` token) captured when the subscription started. Subgraphs requiring short-lived per-connection credentials may reject a reconnect after the original token expires. AWS SigV4 request signing is re-applied per attempt with freshly resolved credentials.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9302
