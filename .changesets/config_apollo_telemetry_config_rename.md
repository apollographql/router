###  ([#5807](https://github.com/apollographql/router/pull/5807))

All known issues related to the new Apollo usage report generation have been resolved so we are renaming some experimental options to be non-experimental.
* experimental_apollo_metrics_generation_mode is now apollo_metrics_generation_mode
* telemetry.apollo.experimental_apollo_signature_normalization_algorithm is now telemetry.apollo.signature_normalization_algorithm
* telemetry.apollo.experimental_apollo_metrics_reference_mode is now telemetry.apollo.metrics_reference_mode

Previous configuration will warn but still work.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/5807