### Fix tls connections hanging when any connection does not complete handshake ([PR #8779](https://github.com/apollographql/router/pull/8779))

Ensures the main router listener loop is not blocked when waiting for a TLS handshake to complete. Uses the header read timeout value as a maximum time to wait for the handshake to complete.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8779
