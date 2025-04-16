### Remove "setting resource attributes is not allowed" warning ([PR #7272](https://github.com/apollographql/router/pull/7272))

If Uplink is enabled, Router 2.1.x emits this warning at startup event though no user configuration or other choice is responsible for it:

```
WARN  setting resource attributes is not allowed for Apollo telemetry
```

This removes the warning entirely as it’s not particularly helpful.

Reproduction:

```
APOLLO_KEY=secret APOLLO_GRAPH_REF=starstuff@current cargo run
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/7272
