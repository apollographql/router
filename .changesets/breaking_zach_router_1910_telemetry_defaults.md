### Enable subgraph metrics and extended error metrics by default

Router 3.0 changes the defaults for two Apollo telemetry settings to match the preferred configuration for GraphOS Studio users, and promotes one of them out of preview.

**`telemetry.apollo.subgraph_metrics` now defaults to `true`.** Subgraph metrics send additional per-subgraph operation metrics to GraphOS Studio via OTLP, powering subgraph insights. Previously this was opt-in. To restore the previous behavior, set:

```yaml
telemetry:
  apollo:
    subgraph_metrics: false
```

**`telemetry.apollo.errors.preview_extended_error_metrics` has been renamed to `telemetry.apollo.errors.extended_error_metrics` and now defaults to `enabled`.** Extended error metrics send OTLP error metrics with additional dimensions (`extensions.service`, `extensions.code`), giving Studio richer error attribution out of the box. The `preview_` prefix has been dropped now that the feature is stable.

Configurations using the old `preview_extended_error_metrics` field name are migrated automatically at startup (with a warning). To restore the previous behavior, set:

```yaml
telemetry:
  apollo:
    errors:
      extended_error_metrics: disabled
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9879
