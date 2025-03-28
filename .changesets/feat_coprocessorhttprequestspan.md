### Instrument coprocessor request with http_request span ([Issue #6739](https://github.com/apollographql/router/issues/6739))

Coprocessor requests will now emit an `http_request` span. This span can help to gain
insight into latency that may be introduced over the network stack when communicating with coprocessor. 

Coprocessor span attributes are:
* `otel.kind`: `CLIENT`
* `http.request.method`: `POST`
* `server.address`: `<target address>`
* `server.port`: `<target port>`
* `url.full`: `<url.full>`
* `otel.name`: `<method> <url.full>`
* `otel.original_name`: `http_request`

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/6776
