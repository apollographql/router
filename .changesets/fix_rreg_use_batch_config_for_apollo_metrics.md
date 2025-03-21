### Use batch_processor config for Apollo metrics PeriodicReader ([PR #7024](https://github.com/apollographql/router/pull/7024))

The Apollo otlp `batch_processor` configurations `telemetry.apollo.batch_processor.scheduled_delay` and `telemetry.apollo.batch_processor.max_export_timeout` now also control the Apollo otlp `PeriodicReader` export interval and timeout respectively. This brings parity between Apollo otlp metrics and [non-Apollo otlp Exporter metrics](https://github.com/apollographql/router/blob/0f88850e0b164d12c14b1f05b0043076f21a3b28/apollo-router/src/plugins/telemetry/metrics/otlp.rs#L37-L40)

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/7024
