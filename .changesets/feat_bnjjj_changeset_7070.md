### Add support of ignored_headers for subscription deduplication ([PR #7070](https://github.com/apollographql/router/pull/7070))

Add support for `ignored_headers` for subscription deduplication which is a way to ignore some specific headers in deduplication because for example if you include some transaction ID in headers or so , then you can’t benefit from subscription dedup even if it doesn't change the data returned by susbscription.

Here is an example of configuration:

```yaml
subscription:
  enabled: true
  deduplication:
    enabled: true # optional, default: true
    ignored_headers: # (optional) List of ignored headers when deduplicating subscriptions
    - x-transaction-id
    - custom_header_name
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7070