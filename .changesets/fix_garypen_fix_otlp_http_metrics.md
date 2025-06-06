### Fix otlp metric export when using http protocol ([PR #7595](https://github.com/apollographql/router/pull/7595))

We updated the router dependency for opentelemetry when we released router 2.0.

The opentelemetry dependency changed how it processed endpoints (destinations for metrics and traces) and this was not detected until now.

The router wasn't setting the path correctly, so exporting metrics over http was not working for the default endpoint. Exporting metrics via gRPC was not impacted. Neither were traces.

We have fixed our interactions with the dependency and improved our testing to make sure this does not occur again.

The router now supports setting standard OTEL environment variables for endpoints. However, there is a known problem when using environment variables to configure endpoints for the http protocol. If any of:

OTEL_EXPORTER_OTLP_ENDPOINT
OTEL_EXPORTER_OTLP_METRICS_ENDPOINT
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT

are set, then you will see log messages which look like:

```
2025-06-06T15:12:47.992144Z ERROR  OpenTelemetry metric error occurred: Metrics exporter otlp failed with the grpc server returns error (Unknown error): , detailed error message: h2 protocol error: http2 error tonic::transport::Error(Transport, hyper::Error(Http2, Error { kind: GoAway(b"", FRAME_SIZE_ERROR, Library) }))
2025-06-06T15:12:47.992763Z ERROR  OpenTelemetry trace error occurred: Exporter otlp encountered the following error(s): the grpc server returns error (Unknown error): , detailed error message: h2 protocol error: http2 error tonic::transport::Error(Transport, hyper::Error(Http2, Error { kind: GoAway(b"", FRAME_SIZE_ERROR, Library) }))
```

The traces and metrics are processed and delivered correctly to the specified endpoint regardless of this message.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7595
