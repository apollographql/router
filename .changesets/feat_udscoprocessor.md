### Add support for coprocessor communication over Unix Domain Sockets ([Issue #5739](https://github.com/apollographql/router/issues/5739))

Many of Apollo's customers using coprocessor will co-locate their coprocessor with their Router instance on the same host. 
(ie: within the same pod in kubernetes).

This PR brings parity to coprocessor communication with the Unix Domain Sockets support that subgraphs have had for some time. 
Bypassing the full tcp/ip network stack and allowing data transfer between Router and the coprocessor using memory space can reduce latency compared to HTTP.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8348