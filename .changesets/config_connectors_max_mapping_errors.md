### Add `limits.connector.max_mapping_errors` to bound connector mapping errors

Connector response mappings can report errors in the response's `extensions.connectorErrors` with the [`->withError` mapping method](https://www.apollographql.com/docs/graphos/connectors/responses/error-handling). A `->withError` inside a `->map` records one error per element, so a mapping over a large API response can contribute one error per row.

A new limit caps how many such errors one connector response may contribute, alongside the existing `http_max_response_size`:

```yaml title="router.yaml"
limits:
  connector:
    all:
      max_mapping_errors: 100 # at most 100 mapping errors per connector response
    sources:
      my_subgraph.my_api:
        max_mapping_errors: 20 # per-source override
```

As with `http_max_response_size`, a per-source entry under `sources` takes precedence over `all`, and sources are identified by `subgraph_name.source_name`.

Errors past the limit are replaced by a single summary error carrying the `CONNECTORS_TOO_MANY_ERRORS` code and stating how many were dropped, so a truncated list is visible in the response rather than silent. The router also increments the `apollo.router.limits.connector_mapping_errors.exceeded` counter, with a `connector.source` attribute identifying the affected source.

The default is no limit: every declared error is reported, matching how the router passes through subgraph errors. The limit applies only to errors a mapping declares with `->withError`; mapping *problems* — the mapping language's own diagnostics — are never sent to clients and are unaffected.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
