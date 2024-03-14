### Unix socket support for subgraphs ([Issue #3504](https://github.com/apollographql/router/issues/3504))

The Router now supports Unix sockets for subgraph connections by specifying URLs in the `unix:///path/to/router.sock` format in the schema. The Router will use stream unix sockets, not datagram ones. It supports compression but not TLS.
Due to the lack of standard for unix socket URLs, and lack of support in the common URL types in Rust, a transformation is applied to to the socket path to parse it: it is encoded in hexadecimal and stored in the authority part. This will have no consequence on the way the router works, but subgraph services will see URLs with the hex encoded host.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4757