### Support Unix domain socket (UDS) communication for coprocessors ([Issue #5739](https://github.com/apollographql/router/issues/5739))

Many of Apollo’s coprocessor users deploy the coprocessor side‑by‑side with the Router, typically on the same host (for example, within the same Kubernetes pod).

This feature brings coprocessor communication to parity with subgraphs by adding Unix domain socket (UDS) support. When the Router and coprocessor are co‑located, communicating over a Unix domain socket bypasses the full TCP/IP network stack and uses shared host memory instead, which can meaningfully reduce latency compared to HTTP.
By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8348