### Add response body telemetry selector ([PR #7363](https://github.com/apollographql/router/pull/7363))

The Router now supports a `response_body` selector which provides access to the response body in telemetry configurations.
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
