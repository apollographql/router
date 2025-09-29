### Include invalid Trace ID values in error logs ([PR #8149](https://github.com/apollographql/router/pull/8149))

Error messages for malformed Trace IDs now include the invalid value to help with debugging. Previously, when the router received an unparseable Trace ID in incoming requests, error logs only indicated that the Trace ID was invalid without showing the actual value.

Trace IDs can be unparseable due to invalid hexadecimal characters, incorrect length, or non-standard formats. Including the invalid value in error logs makes it easier to diagnose and resolve tracing configuration issues.

By [@juancarlosjr97](https://github.com/juancarlosjr97) in https://github.com/apollographql/router/pull/8149
