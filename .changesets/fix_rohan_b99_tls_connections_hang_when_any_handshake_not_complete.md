### Fix tls connections hanging when any connection does not complete handshake ([PR #8779](https://github.com/apollographql/router/pull/8779))

Ensures the main router listener loop is not blocked when waiting for a TLS handshake to complete. Uses a new config variable `server.http.tls_handshake_timeout` to control how long to wait before terminating a connection, defaulting to 10s.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8779
