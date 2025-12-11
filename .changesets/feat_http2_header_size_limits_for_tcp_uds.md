### Enable HTTP/2 header size limits for TCP and UDS ([PR #8673](https://github.com/apollographql/router/pull/8673))

The router's HTTP/2 header size limit configuration option now applies to requests using TCP and UDS (Unix domain sockets). Previously, this setting only worked for TLS connections.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8673
