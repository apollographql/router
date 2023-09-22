### Fix Studio reporting when it is not configured ([Issue #3871](https://github.com/apollographql/router/issues/3871))

Apollo Studio reporting was broken in 1.30.0 if the Apollo exporter was not configured. If the configuration file contained anything under:

```yaml
telemetry:
  apollo:
```

then it would have been working properly. It is now working in all cases by properly detecting the presence of the Apollo key and graph reference

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3881