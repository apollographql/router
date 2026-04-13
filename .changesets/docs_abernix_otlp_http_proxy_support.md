### Document HTTP proxy support for GraphOS OTLP exporters ([PR #9055](https://github.com/apollographql/router/pull/9055))

Documents that the router's GraphOS OTLP exporters respect the standard `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` environment variables when using the HTTP transport. Includes a note that TLS-inspecting proxies also require the proxy's root certificate to be added to the router's trust store.

Also corrects the minimum version badge for `experimental_otlp_tracing_protocol` / `experimental_otlp_metrics_protocol` to Router v1.49.0, which is when HTTP transport support was first introduced.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/9152
