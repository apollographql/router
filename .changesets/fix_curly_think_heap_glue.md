### Helm: Correct default telemetry `resource` property in `ConfigMap` (copy #6105) ([Issue #6104](https://github.com/apollographql/router/issues/6104))

The Helm chart was using an outdated value when emitting the `telemetry.exporters.metrics.common.resource.service.name` values.  This has been updated to use the correct (singular) version of `resource` (rather than the incorrect `resources` which was used earlier in 1.x's life-cycle).

By [@vatsalpatel](https://github.com/vatsalpatel) in https://github.com/apollographql/router/pull/6105