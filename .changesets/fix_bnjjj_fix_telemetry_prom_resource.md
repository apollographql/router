### Telemetry: export properly resources on metrics configured on prometheus ([PR #7394](https://github.com/apollographql/router/pull/7394))

When configuring `resource` to globally add labels on metrics like this:

```yaml
telemetry:
  apollo:
    client_name_header: name_header
    client_version_header: version_header
  exporters:
    metrics:
      common:
        resource:
          "test-resource": "test"
      prometheus:
        enabled: true
```

`test-resource` label was never exported to prometheus, this bug only occurs with prometheus and not otlp. 
This PR fixes this behavior and will no longer filter `resource`s.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7394