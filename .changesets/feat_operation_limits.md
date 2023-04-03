### GraphOS Enterprise: operation limits

You can define [operation limits](https://www.apollographql.com/docs/router/configuration/operation-limits) in your router's configuration to reject potentially malicious requests. An operation that exceeds _any_ specified limit is rejected.

You define operation limits in your router's [YAML config file](https://www.apollographql.com/docs/router/configuration/overview#yaml-config-file), like so:

```yaml
supergraph:
  limits:
    max_depth: 100
    max_height: 200
    max_aliases: 30
    max_root_fields: 20
```

See details in [operation limits documentation](https://www.apollographql.com/docs/router/configuration/operation-limits).

By [@SimonSapin](https://github.com/SimonSapin), [@lrlna](https://github.com/lrlna), and [@StephenBarlow](https://github.com/StephenBarlow)
