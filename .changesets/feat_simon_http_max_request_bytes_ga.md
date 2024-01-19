### Promote HTTP request size limit from experimental to general availability ([PR #4442](https://github.com/apollographql/router/pull/4442))

In this release, the router YAML configuration option to set the maximum size of an HTTP  request body is promoted [from experimental to general availability](https://www.apollographql.com/docs/resources/product-launch-stages/). The option was previously `experimental_http_max_request_bytes` and is now `http_max_request_bytes`.

The previous `experimental_http_max_request_bytes` option works but produces a warning.

To migrate, rename `experimental_http_max_request_bytes` to the generally available `http_max_request_bytes` option: 

```yaml
limits:
  http_max_request_bytes: 2000000 # Default value: 2 MB
```

By default, the Apollo Router limits the size of the HTTP request body it reads from the network to 2 MB. Before increasing this limit, consider testing performance in an environment similar to your production, especially if some clients are untrusted. Many concurrent large requests can cause the router to run out of memory.


By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/4442
