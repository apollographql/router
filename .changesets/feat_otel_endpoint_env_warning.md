### Add startup warning when OTEL_EXPORTER_OTLP_ENDPOINT environment variable is set

When starting the router, a warning is now displayed if the `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable is set. This environment variable takes precedence over default configurations and may override trace export to Apollo Studio.

This warning helps users understand that their telemetry configuration may not be sending data where expected when this environment variable is present.

