### Restrict custom instrument `value`s to relevant stages ([PR #5472](https://github.com/apollographql/router/pull/5472))

Previously, custom instruments at each [request lifecycle stage](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/instruments/#router-request-lifecycle-services) could specify unrelated values, like using `event_unit` for a router instrument. Now, only relevant values for each stage are allowed.

Additionally, GraphQL instruments no longer need to specify `field_event`. There is no automatic migration for this change since GraphQL instruments are still experimental.

```yaml
telemetry:
  instrumentation:
    instruments:
      graphql:
        # OLD definition of a custom instrument that measures the number of fields
        my.unit.instrument:
          value: field_unit # Changes to unit
        
        # NEW definition
        my.unit.instrument:
          value: unit 

        # OLD  
        my.custom.instrument:
          value: # Changes to not require `field_custom`
            field_custom:
              list_length: value
        # NEW
        my.custom.instrument:
          value: 
            list_length: value
```

The following misconfiguration is now not possible:
```yaml
router_instrument:
  value:
    event_custom:
      request_header: foo
```


By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5472
