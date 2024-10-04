### promote experimental_otlp_tracing_sampler from experimental ([PR #6070](https://github.com/apollographql/router/pull/6070))

The router's otlp tracing sampler feature that was previously [experimental](https://www.apollographql.com/docs/resources/product-launch-stages/#experimental-features) is now [generally available](https://www.apollographql.com/docs/resources/product-launch-stages/#general-availability).

If you used its experimental configuration, you should migrate to the new configuration option:

* `telemetry.apollo.experimental_otlp_tracing_sampler` is now `telemetry.apollo.otlp_tracing_sampler`

The experimental configuration option is now deprecated. It remains functional but will log warnings.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6070
