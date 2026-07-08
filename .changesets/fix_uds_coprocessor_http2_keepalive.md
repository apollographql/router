### HTTP/2 keep-alive now works for coprocessors over a Unix domain socket ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

`experimental_http2_keep_alive_interval` (and its companion timeout) were only applied to the TCP client, so they were silently ignored for coprocessors and subgraphs reached over a `unix://` URL. Against a hung-but-open peer — one whose socket stays open with no `EOF` to detect, e.g. during a long stop-the-world pause — the router would wait indefinitely instead of tearing the connection down.

The Unix-socket client now receives the same keep-alive configuration as the TCP client, so unanswered pings trip the keep-alive timeout and the connection is closed after roughly `interval + timeout`.

Because a Unix socket has no TLS/ALPN for the client to negotiate HTTP/2, keep-alive over a socket only takes effect when HTTP/2 is forced on. A coprocessor configured with keep-alive against a `unix://` URL while `experimental_http2` is not `http2only` is now rejected at startup with an actionable error, rather than accepted with keep-alive silently doing nothing.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/PULL_NUMBER
