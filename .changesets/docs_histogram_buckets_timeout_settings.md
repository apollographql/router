### Document default histogram buckets and their relationship to timeout settings

Added documentation explaining how histogram bucket configuration affects timeout monitoring in Prometheus and other metrics exporters.

The documentation now includes:

- **Default bucket values**: Documents the router's default histogram buckets (0.001 to 10.0 seconds)
- **Timeout behavior**: Explains how histogram metrics cap values at the highest bucket boundary, which can make timeouts appear unrespected if they exceed ten seconds
- **Customization guidance**: Shows how to configure custom buckets via `telemetry.exporters.metrics.common.buckets` to match application timeout requirements

This update helps users understand why their timeout metrics may not behave as expected and provides clear guidance on customizing buckets for applications with longer timeout configurations.

By [@the-gigi-apollo](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8783
