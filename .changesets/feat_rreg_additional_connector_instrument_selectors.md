### Additional Connector Custom Instrument Selectors ([PR #8045](https://github.com/apollographql/router/pull/8045))

This adds new [custom instrument selectors](https://www.apollographql.com/docs/graphos/routing/observability/telemetry/instrumentation/selectors#connector) for Connectors and enhances some existing selectors. The new selectors are:
 - `supergraph_operation_name`
 - `supergraph_operation_kind`
 - `request_context`

These selectors were modified to add additional functionality:
 - `connector_request_mapping_problems`
   - Adds a new `boolean` variant that will return `true` when a mapping problem exists on the request
 - `connector_response_mapping_problems`
   - Adds a new `boolean` variant that will return `true` when a mapping problem exists on the response
 - `error`
   - Adds another trigger for when the HTTP response contains an error. Previously only critical errors were covered.
   - Adds a new `boolean` variant that will return `true` when an error exists on the response or a critical error ocurred.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/8045