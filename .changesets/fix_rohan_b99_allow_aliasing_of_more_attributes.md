### Ensure `client.name`, `client.version`, `http.route`, and `http.request.method` can be aliased on router spans ([PR #9048](https://github.com/apollographql/router/pull/9048))


`client.name` and `client.version` have now been added to `RouterAttributes`, `http.route` has been added to `HttpServerAttributes`, and `http.request.method` has been added to `HttpCommonAttributes`. The default behavior should remain the same. Example configuration using aliases:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          http.route:
            alias: http_route
          http.request.method:
            alias: http_request_method
```


By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9048
