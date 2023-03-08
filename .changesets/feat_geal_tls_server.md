### TLS server support ([Issue #2615](https://github.com/apollographql/router/issues/2615))

The Router has to provide a TLS server to support HTTP/2 on the client side. This uses the rustls implementation (no TLS versions below 1.2), limited to one server certificate and safe default ciphers.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2614