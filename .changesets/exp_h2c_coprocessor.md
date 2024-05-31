### Add experimental h2c communication capability for communicating with coprocessor ([Issue #5299](https://github.com/apollographql/router/issues/5299))

Allow customers to have more control over the communication with their coprocessor, specifically enabling use of h2c (http2 cleartext) communication for scenarios where the networking architecture/mesh connections do not support or require tls for outbound communications from router.

Introduces a new config `traffic_shaping` under coprocessor, where the first and only supported option is `experimental_http2`, having the same options available as `experimental_http2` option under subgraphs:

- disable - disable http2, use http/1.1 only
- enable - http urls results in http/1.1, https urls results in tls with either http1.1 or http2 based on the tls handshake 
- http2only - http urls result in h2c, https urls result in tls with http2
- not set - defaults to enable

ⓘ NOTE Configuring experimental_http2: http2only where the network doesn't support http2 will result in a failed coprocessor connection.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/5300