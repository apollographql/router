### Allow overriding of service version ([PR #5689](https://github.com/apollographql/router/pull/5689))

Previously `service.version` was not overridable via yaml and was ignored. It is now possible to set this explicitly which can be useful for users producing custom builds of the Router.

The following example overrides the version to be `1.0`:
```yaml
telemetry:
  exporters:
    tracing:
      common:
        resource:
          service.version: 1.0
```


By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5689
