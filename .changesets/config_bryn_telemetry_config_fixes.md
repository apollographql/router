### Bring telemetry tracing config and metrics config into alignment ([Issue #4043](https://github.com/apollographql/router/issues/4043))

Configuration between tracing and metrics was inconsistent and did not align with otel spec terminology. The following changes have been made to router.yaml configuration:

`trace_config` has been renamed to `common`
  
  ```diff
telemetry
  tracing:
-   trace_config:
+   common:   
  ```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4044
