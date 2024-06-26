### Add experimental h2c communication capability for communicating with coprocessor ([Issue #5299](https://github.com/apollographql/router/issues/5299))

Allows HTTP/2 Cleartext (h2c) communication with coprocessors for scenarios where the networking architecture/mesh connections don't support or require TLS for outbound communications from the router.

Introduces a new `coprocessor.client` configuration. The first and currently only option is `experimental_http2`. The available option settings are the same as the as [`experimental_http2` traffic shaping settings](https://www.apollographql.com/docs/router/configuration/traffic-shaping/#http2).

- `disable` - disable HTTP/2, use HTTP/1.1 only
- `enable` - HTTP URLs use HTTP/1.1, HTTPS URLs use TLS with either HTTP/1.1 or HTTP/2 based on the TLS handshake 
- `http2only` - HTTP URLs use h2c, HTTPS URLs use TLS with HTTP/2
- not set - defaults to `enable`

ⓘ NOTE Configuring `experimental_http2: http2only` where the network doesn't support HTTP2 results in a failed coprocessor connection.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/5300