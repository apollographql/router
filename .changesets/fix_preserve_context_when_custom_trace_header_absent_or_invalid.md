### Preserve existing trace context when custom trace header is absent or invalid

When `propagation.request.header_name` is configured alongside `trace_context: true`,
requests that send only a standard `traceparent` header (without the custom header)
would have their trace context silently overwritten with an empty context.

This fix ensures that when the custom header is absent or contains an invalid value,
the existing trace context (e.g. extracted from `traceparent`) is preserved instead
of being overwritten with an empty context.

By [@p623](https://github.com/p623) in https://github.com/apollographql/router/pull/9971