### Block Router startup when certain OTEL environment variables are set (PR #8915￼)

The Apollo Router will now *fail to start* if any of the following OpenTelemetry (OTEL) keyword environment variables are set:
	•	OTEL_EXPORTER_OTLP_ENDPOINT
	•	OTEL_EXPORTER_OTLP_TRACES_ENDPOINT
	•	OTEL_EXPORTER_OTLP_METRICS_ENDPOINT
  
Using these variables *is not supported by the Router* since it can override or interfere with its built-in telemetry configuration, leading to unexpected behavior.

Previously, the Router emitted a warning when OTEL_EXPORTER_OTLP_ENDPOINT was present. With this change, *startup is now blocked* to prevent unintended telemetry configuration conflicts.

If your deployment defines any of these environment variables (for example, through base container images, platform defaults, or infrastructure tooling), they must be removed before starting the Router.
