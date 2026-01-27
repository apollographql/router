### Document default histogram buckets and their relationship to timeout settings ([PR #8783](https://github.com/apollographql/router/pull/8783))

The documentation now explains how histogram bucket configuration affects timeout monitoring in Prometheus and other metrics exporters.

The documentation now includes:

- Default bucket values: The router's default histogram buckets (`0.001` to `10.0` seconds)
- Timeout behavior: Histogram metrics cap values at the highest bucket boundary, which can make timeouts appear ignored if they exceed ten seconds
- Customization guidance: Configure custom buckets via `telemetry.exporters.metrics.common.buckets` to match your timeout settings

This update helps users understand why their timeout metrics may not behave as expected and provides clear guidance on customizing buckets for applications with longer timeout configurations.

By [@the-gigi-apollo](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8783
