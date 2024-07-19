### Allow overriding of service version ([PR #5689](https://github.com/apollographql/router/pull/5689))

This PR allows yaml configured service.version to override the built in version that is baked into the router.

For example:
```yaml
telemetry:
  exporters:
    tracing:
      common:
        resource:
          service.version: 1.0
```

Overrides the version to `1.0`.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5689
