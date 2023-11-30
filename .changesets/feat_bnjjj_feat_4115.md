### feat(subscription): Support configurable heartbeat for subscription callback protocol ([Issue #4115](https://github.com/apollographql/router/issues/4115))

The heartbeat interval that the Apollo Router uses for the subscription callback protocol is now configurable.

The heartbeat can even be disabled for certain platforms.

An example configuration:

```yaml
subscription:
  enabled: true
  mode:
    preview_callback:
      public_url: http://127.0.0.1:4000
      heartbeat_interval: 5s # Optional
      listen: 127.0.0.1:4000
      path: /callback
      subgraphs:
      - accounts
 ```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4246
