### Configure maximum HTTP/2 header list size ([PR #8636](https://github.com/apollographql/router/pull/8636))

The router now supports configuring the maximum size for HTTP/2 header lists via the `limits.http2_max_headers_list_bytes` setting. This protects against excessive resource usage from clients sending large sets of HTTP/2 headers.

The default remains 16KiB. When a client sends a request with HTTP/2 headers whose total size exceeds the configured limit, the router rejects the request with a 431 error code.

**Example configuration:**

```yaml
limits:
  http2_max_headers_list_bytes: "48KiB"
```

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8636
