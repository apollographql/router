### Extended references in Apollo usage reports ([Issue #ROUTER-360](https://apollographql.atlassian.net/browse/ROUTER-360))

Adds a new experimental configuration option to turn on extended references in Apollo usage reports, including references to input object fields and enum values.

This new configuration (telemetry.apollo.experimental_apollo_metrics_reference_mode: extended) only works when in `experimental_apollo_metrics_generation_mode: new` mode and we don't yet recommend enabling it while we continue to verify that the new functionality works as expected.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/5331