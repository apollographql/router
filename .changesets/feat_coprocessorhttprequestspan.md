### Instrument coprocessor request with http_request span ([Issue #6739](https://github.com/apollographql/router/issues/6739))

Instrument coprocessor http_requests with spans, identically to what is done for subgraph http_requests.  Helps provide
insight into latency that may be introduced over the network stack when communicating with coprocessor. 

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/6776
