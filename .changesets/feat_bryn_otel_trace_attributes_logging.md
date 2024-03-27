### Enable displaying trace and span id on logs ([PR #4823](https://github.com/apollographql/router/pull/4823))

To enable correlation between trace and logs `trace_id` and `span_id` can now be included on the log messages.

 Json:
 ```
 {"timestamp":"2024-03-19T15:37:41.516453239Z","level":"INFO","trace_id":"54ac7e5f0e8ab90ae67b822e95ffcbb8","span_id":"9b3f88c602de0ceb","message":"Supergraph GraphQL response".....
 ```

 Text:
 ```
 2024-03-19T15:14:46.040435Z INFO trace_id: bbafc3f048b6137375dd78c10df18f50 span_id: 40ede28c5df1b5cc router{
 ```

To configure this, use `display_span_id` and `display_trace_id` options in the logging exporter configuration.

Json (defaults to true):
```
telemetry:
  exporters:
    logging:
      stdout:
        format:
          json:
            display_span_id: true
            display_trace_id: true
```

Text (defaults to false):
```
telemetry:
  exporters:
    logging:
      stdout:
        format:
          text:
            display_span_id: false
            display_trace_id: false
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4823
