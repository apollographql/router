### Apollo reporting signature enhancements ([PR #5062](https://github.com/apollographql/router/pull/5061))

Adds a new experimental configuration option to turn on some enhancements for the Apollo reporting stats report key:
* Signatures will include the full normalized form of input objects
* Signatures will include aliases
* Some small normalization improvements

This new configuration (telemetry.apollo.experimental_apollo_signature_normalization_algorithm) only works when in `experimental_apollo_metrics_generation_mode: new` mode and we don't yet recommend enabling it while we continue to verify that the new functionality works as expected.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/5062