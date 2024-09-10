###  General availability of Apollo usage report generation ([#5807](https://github.com/apollographql/router/pull/5807))

The router's Apollo usage report generation feature that was previously [experimental](https://www.apollographql.com/docs/resources/product-launch-stages/#experimental-features) is now [generally available](https://www.apollographql.com/docs/resources/product-launch-stages/#general-availability).

If you used its experimental configuration, you should migrate to the new configuration options:

* `telemetry.apollo.experimental_apollo_metrics_reference_mode` is now `telemetry.apollo.metrics_reference_mode`
* `telemetry.apollo.experimental_apollo_signature_normalization_algorithm` is now `telemetry.apollo.signature_normalization_algorithm`
* `experimental_apollo_metrics_generation_mode` has been removed because the Rust implementation (the default since router v1.49.0) is generating reports identical to the previous router-bridge implementation

The experimental configuration options are now deprecated. They are functional but will log warnings.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/5807