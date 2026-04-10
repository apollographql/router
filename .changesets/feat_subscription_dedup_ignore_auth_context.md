### Add `ignore_auth_context` option to subscription deduplication config ([PR #9078](https://github.com/apollographql/router/pull/9078))

When the router's JWT authentication plugin validates a token, it decodes the claims and stores them internally on the request — before any subgraph request is built.  The router then factors those stored claims into its check for whether two subscriptions are identical, separately from any HTTP headers it may forward downstream.

This means that on any router with JWT authentication enabled, every authenticated user effectively gets their own subgraph WebSocket connection — even if the subscription data is identical for all users, and even if the `Authorization` header is never forwarded to the subgraph at all.  Adding `authorization` to `ignored_headers` doesn't help here, because it only affects HTTP headers; the decoded claims live in a different layer that `ignored_headers` never touches.

Two new capabilities are added to the `deduplication` config block:

- `ignore_auth_context: bool` (default: `false`) — when `true`, the router skips stored JWT claims when checking subscription identity, allowing all authenticated users to share a single subgraph WebSocket connection when the subscription data is truly non-personalized (e.g., product price updates, stock price feeds).
- Per-subgraph deduplication control via `all:` / `subgraphs:` — deduplication settings can now be set globally with a default and overridden per subgraph by name, using the standard `SubgraphConfiguration<T>` pattern already used elsewhere in the router config.

```yaml
subscription:
  deduplication:
    all:
      enabled: true
      ignore_auth_context: true
    subgraphs:
      article:
        enabled: false
```

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/9078
