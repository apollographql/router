### Improve error logging for malformed Trace IDs ([PR #8149](https://github.com/apollographql/router/pull/8149))

When the router receives an unparseable Trace ID in incoming requests, the logged error message now includes the invalid value. Trace IDs can be unparseable due to invalid hexadecimal characters, incorrect length, or non-standard formats.

By [@juancarlosjr97](https://github.com/juancarlosjr97) in https://github.com/apollographql/router/pull/8149
