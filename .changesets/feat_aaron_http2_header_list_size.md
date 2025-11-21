### feat(http2): configure max allowed header list size ([PR #8636](https://github.com/apollographql/router/pull/8636))

The Router now supports configuring the maximum size allowed for HTTP/2 header lists with the `limits.http2_max_headers_list_bytes` setting. This protects against excessive resource usage from extremely large sets of HTTP/2 headers sent by clients.

**Example configuration:**

```yaml
limits:
  http2_max_headers_list_bytes: "20Mib"
```

If a client sends a request with HTTP/2 headers whose total size exceeds the configured `http2_max_headers_list_bytes`, the request will be rejected with a 431 error code.


By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8636
