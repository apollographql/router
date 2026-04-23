### Add `operation_body_timeout` for file upload requests ([PR #9243](https://github.com/apollographql/router/pull/9243))

Adds a new `operation_body_timeout` limit to the file uploads plugin, allowing operators to set a tight deadline on reading the operations field (GraphQL query + variables) from multipart request bodies, independently of the overall router `timeout`.

File uploads is the only router flow where the request body is read as a stream in the plugin layer: the multipart body must be parsed to extract the operations field before query planning can begin. This means a slow or stalled client can hold a connection open until the global router `timeout` fires. The new `operation_body_timeout` lets you set a tighter deadline specifically for that body-reading phase.

If `operation_body_timeout` is not set, no additional body-read timeout is applied — the overall router `timeout` remains the only bound.

```yaml
preview_file_uploads:
  enabled: true
  protocols:
    multipart:
      enabled: true
      limits:
        operation_body_timeout: 5s  # optional; no default
```

When the timeout fires, the router returns a `504 Gateway Timeout` response with extension code `GATEWAY_TIMEOUT`.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9243
