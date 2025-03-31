### Add support for connector header propagation via YAML config ([PR #7152](https://github.com/apollographql/router/pull/7152))

Added support for connector header propagation via YAML config. All of the existing header propagation in the Router now works for connectors by using
`headers.connector.all` to apply rules to all connectors or `headers.connector.sources.*` to apply rules to specific sources.

Note that if one of these rules conflicts with a header set in your schema, either in `@connect` or `@source`, the value in your Router config will
take priority and be treated as an override.

```
headers:
  connector:
    all:
    request:
        - insert:
            name: "x-inserted-header"
            value: "hello world!"
        - propagate:
            named: "x-client-header"
    sources:
      connector-graph.random_person_api:
        request:
          - insert:
              name: "x-inserted-header"
              value: "hello world!"
          - propagate:
              named: "x-client-header"
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7152
