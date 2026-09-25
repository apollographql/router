### Add the `->withWarning` and `->withError` mapping methods for connectors

Connector mappings can now flag a suspect value without failing the field. Both methods pass their input through unchanged:

- `->withWarning("...")` records a diagnostic for the mapping author, visible in the connectors debugger and the `connector_response_mapping_problems` telemetry selector. It never reaches clients.
- `->withError("...")` (or `->withError({ message, extensions })`) reports an error to the client in the response's `extensions.connectorErrors` array, alongside the data. It is counted by the `apollo.router.graphql_error` metric.

```graphql
availability: stock_code->match(
  ["A", "IN_STOCK"],
  [@, @->withError("Unrecognized stock code")]
)
```

Declared errors follow the [`include_subgraph_errors`](https://www.apollographql.com/docs/graphos/routing/observability/subgraph-error-inclusion) setting for the connector's subgraph, so they are redacted by default. See [Connectors error handling](https://www.apollographql.com/docs/graphos/connectors/responses/error-handling#report-errors-without-failing-a-field) for details.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050 and [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
