### Experimental Configuration Options for HTTP/1 Max Headers and Buffer Limits ([PR #6194](https://github.com/apollographql/router/pull/6194))

This update introduces experimental configuration options that allow you to adjust the maximum number of HTTP/1 request headers and the maximum buffer size allocated for headers.

By default, the Router accepts HTTP/1 requests with up to 100 headers and allocates ~400kib of buffer space to store them. If you need to handle requests with more headers or require a different buffer size, you can now configure these limits in the Router's configuration file:
```yaml
limits:
  experimental_http1_request_max_headers: 200
  experimental_http1_request_max_buf_size: 200kib
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6194
