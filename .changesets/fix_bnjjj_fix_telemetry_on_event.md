### Fix displaying custom event attributes on subscription events ([PR #6033](https://github.com/apollographql/router/pull/6033))

The router now properly displays custom event attributes that are set with selectors at the supergraph level. 

An example configuration:

```yaml title=router.yaml
telemetry:
  instrumentation:
    events:
      supergraph:
        supergraph.event:
          message: supergraph event
          on: event_response # on every supergraph event (like subscription event for example)
          level: info
          attributes:
            test:
              static: foo
            response.data:
              response_data: $ # Display all the response data payload
            response.errors:
              response_errors: $ # Display all the response errors payload
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6033