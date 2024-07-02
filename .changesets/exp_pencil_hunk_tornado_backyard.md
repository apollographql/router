### Add experimental extended reference reporting configuration ([Issue #ROUTER-360](https://apollographql.atlassian.net/browse/ROUTER-360))

Adds an experimental configuration to turn on extended references in Apollo usage reports, including references to input object fields and enum values.

This new configuration (`telemetry.apollo.experimental_apollo_metrics_reference_mode: extended`) only works when `experimental_apollo_metrics_generation_mode: new` is configured.
Apollo doesn't yet recommend these configurations in production while we continue to verify that the new functionality works as expected.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/5331