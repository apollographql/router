### Add support for delta aggregation to otlp metrics ([PR #3412](https://github.com/apollographql/router/pull/3412))

Add a new configuration option (Temporality) to the otlp metrics configuration.

This may be useful to fix problems with metrics when being processed by datadog which tends to expect Delta, rather than Cumulative, aggregations.

See:
 - https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/6129
 - https://github.com/DataDog/documentation/pull/15840

for more details.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3412