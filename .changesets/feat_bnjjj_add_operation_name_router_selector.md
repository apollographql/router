### Add selector for router service in custom telemetry ([PR #5392](https://github.com/apollographql/router/pull/5392))

Instead of having to access to the operation_name using the response_context at the router service, we now provide a selector for operation name at the router service in instrumentations.

example:

```yaml
telemetry:
  instrumentation:
    instruments:
      router:
        http.server.request.duration:
          attributes:
            graphql.operation.name:
              operation_name: string
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5392