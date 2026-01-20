### Clarify traffic shaping compression headers in documentation ([PR #8773](https://github.com/apollographql/router/pull/8773))

The traffic shaping documentation now clearly explains how the router handles HTTP compression headers for subgraph requests. It clarifies that `content-encoding` is set when compression is configured via `traffic_shaping`, while `accept-encoding` is automatically set on all subgraph requests to indicate the router can accept compressed responses (`gzip`, `br`, or `deflate`). The documentation also notes that these headers are added after requests are added to the debug stack, so they won't appear in the Connectors Debugger.

By [@gigi](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8773
