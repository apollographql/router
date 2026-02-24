### Support Unix domain socket (UDS) communication for coprocessors ([Issue #5739](https://github.com/apollographql/router/issues/5739))

Many coprocessor deployments run side-by-side with the router, typically on the same host (for example, within the same Kubernetes pod).

This change brings coprocessor communication to parity with subgraphs by adding Unix domain socket (UDS) support. When the router and coprocessor are co-located, communicating over a Unix domain socket bypasses the full TCP/IP network stack and uses shared host memory instead, which can meaningfully reduce latency compared to HTTP.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8348
