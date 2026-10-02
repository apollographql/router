### Remove deprecated `traffic_shaping.deduplicate_variables` field

Variable deduplication when sending requests to subgraphs is unconditionally enabled, so the `traffic_shaping.deduplicate_variables` config field was accepted but silently ignored. It was deprecated in 2.x and has now been removed. Please remove the field from your config:

```yaml
# No longer supported
traffic_shaping:
  deduplicate_variables: true
```

Configurations that still set the field are migrated automatically at startup with a warning.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9877
