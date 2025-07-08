### Add validation for incompatible Datadog and trace context propagation

Prevents simultaneous activation of Datadog tracing and W3C trace context propagation to avoid trace ID format conflicts. When both are enabled, the router now returns a clear configuration error explaining the incompatibility between Datadog's 64-bit trace IDs and W3C trace context's 128-bit trace IDs.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/7848