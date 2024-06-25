### Make `value` for custom instruments easier to use ([PR #5472](https://github.com/apollographql/router/pull/5472))

Previously every custom instrument at every stage had the ability to specify values that were not related to that stage. e.g. event_unit for a router instrument.

With this change only the relevant values for the stage are allowed. In addition, graphql instruments no longer need to specify field_event:
There is no automatic migration for this as graphql instruments are still experimental.

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
