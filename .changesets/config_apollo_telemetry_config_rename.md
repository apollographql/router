###  ([#5807](https://github.com/apollographql/router/pull/5807))

All known issues related to the new Apollo usage report generation have been resolved so we are renaming some experimental options to be non-experimental.
* `telemetry.apollo.experimental_apollo_metrics_reference_mode` is now `telemetry.apollo.metrics_reference_mode`
* `telemetry.apollo.experimental_apollo_signature_normalization_algorithm` is now `telemetry.apollo.signature_normalization_algorithm`
* `experimental_apollo_metrics_generation_mode` has been removed since the Rust implementation has been the default since v1.49.0 and it is generating reports identical to the router-bridge implementation

Previous configuration will warn but still work.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/5807