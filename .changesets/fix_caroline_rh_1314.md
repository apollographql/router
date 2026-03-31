### Ensure `http2only` uses h2c for cleartext connections ([PR #9018](https://github.com/apollographql/router/pull/9018))

`hyper` does not support [upgrading cleartext connections from HTTP/1.1 to HTTP/2](https://github.com/hyperium/hyper/issues/2411). To use HTTP/2 without TLS, clients must use 'prior knowledge' — connecting with the HTTP/2 preface directly. This is what `experimental_http2: http2only` is for, but previously HTTP/1 was always enabled in the connector, causing the client to fall back to HTTP/1.1 regardless. This fix applies to all outbound HTTP connections: subgraphs, connectors, and coprocessors.

| `experimental_http2` | TLS | protocol used                                 |
|----------------------|-----|-----------------------------------------------|
| `disable`            | yes | HTTP/1.1                                      |
| `disable`            | no  | HTTP/1.1                                      |
| `enable`             | yes | HTTP/2 (if server supports it), else HTTP/1.1 |
| `enable`             | no  | HTTP/1.1                                      |
| `http2only`          | yes | HTTP/2                                        |
| `http2only`          | no  | HTTP/2 (h2c — cleartext prior knowledge)      |

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9018
