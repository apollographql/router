### Configuration options for HTTP/1 max headers and buffer limits ([PR #6194](https://github.com/apollographql/router/pull/6194))

This update introduces configuration options that allow you to adjust the maximum number of HTTP/1 request headers and the maximum buffer size allocated for headers.

By default, the router accepts HTTP/1 requests with up to 100 headers and allocates ~400 KiB of buffer space to store them. If you need to handle requests with more headers or require a different buffer size, you can now configure these limits in the router's configuration file:
```yaml
limits:
  http1_request_max_headers: 200
  http1_request_max_buf_size: 200kib
```

If you are using the router as a Rust crate, the `http1_request_max_buf_size` option requires the `hyper_header_limits` feature and also necessitates using Apollo's fork of the Hyper crate until the [changes are merged upstream](https://github.com/hyperium/hyper/pull/3523).
You can include this fork by adding the following patch to your Cargo.toml file:
```toml
[patch.crates-io]
"hyper" = { git = "https://github.com/apollographql/hyper.git", tag = "header-customizations-20241108" }
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6194
