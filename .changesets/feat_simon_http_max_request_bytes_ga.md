### Promote HTTP request size limit from experimental to general availability ([PR #4442](https://github.com/apollographql/router/pull/4442))

By default Apollo Router limits the size of the HTTP request body it will read from the network to 2 MB. In this version, the YAML configuration to change the limit is promoted from experimental to general availability.

For more information about launch stages, please see the documentation here: https://www.apollographql.com/docs/resources/product-launch-stages/

Before increasing this limit significantly consider testing performance in an environment similar to your production,
especially if some clients are untrusted. Many concurrent large requests could cause the Router to run out of memory.

Previous configuration will warn but still work:

```yaml
limits:
  experimental_http_max_request_bytes: 2000000 # Default value: 2 MB
```

The warning can be fixed by removing the `experimental_` prefix:

```yaml
limits:
  http_max_request_bytes: 2000000 # Default value: 2 MB
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/4442
