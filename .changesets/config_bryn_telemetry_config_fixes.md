### Bring telemetry tracing config and metrics config into alignment ([Issue #4043](https://github.com/apollographql/router/issues/4043))

Configuration between tracing and metrics was inconsistent and did not align with otel spec terminology. The following changes have been made to router.yaml configuration:

`telemetry.tracing.trace_config` has been renamed to `common`
  
```diff
telemetry
  tracing:
-   trace_config:
+   common:   
```

`telemetry.tracing.common.attributes` has been renamed to `resource`
```diff
telemetry
  tracing:
    common:
-      attributes:
+      resource:   
```

`telemetry.metrics.common.resources` has been renamed to `resource`
```diff
telemetry
  metrics:
    common:
-      resources:
+      resource:   
```
`telemetry.tracing.propagation.awsxray` has been renamed to `aws_xray`
```diff
telemetry
  tracing:
    propagation:
-      awsxray: true
+      aws_xray: true
```

The Router will upgrade any existing configuration on startup. However, you should update your configuration to use the new format as soon as possible. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4044, https://github.com/apollographql/router/pull/4050 and https://github.com/apollographql/router/pull/4051
