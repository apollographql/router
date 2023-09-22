### HTTP/2 Cleartext protocol (h2c) support for subgraph connections ([Issue #3535](https://github.com/apollographql/router/issues/3535))

The router can now connect to subgraphs over HTTP/2 Cleartext, which uses the HTTP/2 binary protocol directly over TCP without TLS. To activate it, set the `experimental_http2` option to `http2_only`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3852