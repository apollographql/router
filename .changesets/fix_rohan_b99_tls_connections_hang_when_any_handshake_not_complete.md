### Prevent TLS connections from hanging when a handshake stalls ([PR #8779](https://github.com/apollographql/router/pull/8779))

The router listener loop no longer blocks while waiting for a TLS handshake to complete. Use `server.http.tls_handshake_timeout` to control how long the router waits before terminating a connection (default: `10s`).

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8779
