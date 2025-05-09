### add response body selector ([PR #7363](https://github.com/apollographql/router/pull/7363))

Adds a new response body selector that allows accessing the response body in telemetry configurations.
This enables more detailed monitoring and logging of response data in the Router.

Example configuration:
```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          "my_attribute":
            response_body: true
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7363
