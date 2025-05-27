### Allow ignoring irrelevant headers during subscriptions de-duplication ([PR #7070](https://github.com/apollographql/router/pull/7070))

The router now supports ignoring specific headers when deduplicating requests to subgraphs providing subscription events.. Previously, any differing headers which didn't actually affect the subscription response (e.g., `user-agent`) would prevent or limit the potential of deduplication.

Now, the introduction of the `ignored_headers` option allows you to specify headers to ignore during deduplication, enabling you to benefit from subscription deduplication even when requests include headers with unique or varying values that don't affect the subscription's event data.

Configuration example:

```yaml
subscription:
  enabled: true
  deduplication:
    enabled: true # optional, default: true
    ignored_headers: # (optional) List of ignored headers when deduplicating subscriptions
      - x-transaction-id
      - custom-header-name
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7070