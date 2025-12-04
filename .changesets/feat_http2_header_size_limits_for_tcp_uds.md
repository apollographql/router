### Enables HTTP/2 header size limits for TCP and UDS (unix sockets)

The router config's HTTP/2 header size limit option is now respected by requests using TCP and UDS. Previously it would only work for TLS connections.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8673
