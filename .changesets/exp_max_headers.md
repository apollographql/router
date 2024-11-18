### Experimental Configuration Options for HTTP/1 Max Headers and Buffer Limits ([PR #6194](https://github.com/apollographql/router/pull/6194))

This update introduces experimental configuration options that allow you to adjust the maximum number of HTTP/1 request headers and the maximum buffer size allocated for headers.

Note: These options are marked as experimental not due to instability in their implementation, but because we are currently evaluating whether similar functionality is needed for HTTP/2. If so, we may consider introducing unified options that apply to both protocols.

By default, the Router accepts HTTP/1 requests with up to 100 headers and allocates ~400kib of buffer space to store them. If you need to handle requests with more headers or require a different buffer size, you can now configure these limits in the Router's configuration file:
```yaml
limits:
  experimental_http1_request_max_headers: 200
  experimental_http1_request_max_buf_size: 200kib
```

Note for Rust Crate Users: If you are using the Router as a Rust crate, the `experimental_http1_request_max_buf_size` option requires the `experimental_hyper_header_limits` feature and also necessitates using Apollo's fork of the Hyper crate until the [changes are merged upstream](https://github.com/hyperium/hyper/pull/3523).
You can include this fork by adding the following patch to your Cargo.toml file:
```toml
[patch.crates-io]
"hyper" = { git = "https://github.com/apollographql/hyper.git", tag = "header-customizations-20241108" }
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6194
