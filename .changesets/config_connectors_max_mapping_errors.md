### Add `limits.connector.max_mapping_errors` to bound connector mapping errors

Connector response mappings can report errors in the response's `extensions.connectorErrors` with the [`->withConnectorError` mapping method](https://www.apollographql.com/docs/graphos/connectors/responses/error-handling). A `->withConnectorError` inside a `->map` records one error per element, so a mapping over a large API response can contribute one error per row.

A new limit caps how many such errors one connector response may contribute, alongside the existing `http_max_response_size`:

```yaml title="router.yaml"
limits:
  connector:
    all:
      http_max_response_size: 2000000 # the existing limit, for comparison
      max_mapping_errors: 100 # at most 100 mapping errors per connector response
    sources:
      my_subgraph.my_api:
        max_mapping_errors: 20 # per-source override
```

As with `http_max_response_size`, a per-source entry under `sources` takes precedence over `all`, and sources are identified by `subgraph_name.source_name`.

Errors past the limit are replaced by a single summary error, so a truncated list is visible in the response rather than silent. With `max_mapping_errors: 100` and 250 declared errors, the last entry in `extensions.connectorErrors` is:

```json
{
  "message": "150 more mapping errors were declared by this connector but not reported, out of 250 total, because the configured `limits.connector.max_mapping_errors` is 100",
  "extensions": { "code": "CONNECTORS_TOO_MANY_ERRORS" }
}
```

Truncation is also reported as telemetry. The router increments the `apollo.router.limits.connector_mapping_errors.exceeded` counter, with a `connector.source` attribute identifying the affected source.

Declared errors are observable whether or not a limit truncates them. Every error a mapping declares with `->withError` is counted by `apollo.router.operations.error`, alongside the usual operation and client attributes and an `apollo.router.error.service` attribute naming the connector's subgraph. That is the counter to watch for the volume a connector is producing; `apollo.router.limits.connector_mapping_errors.exceeded` fires only once a response has already been truncated.

The default is no limit: every declared error is reported, matching how the router passes through subgraph errors. The limit applies only to errors a mapping declares with `->withConnectorError`. Diagnostics recorded with `->withError`, and the mapping language's own problems, are never sent to clients and are unaffected.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
