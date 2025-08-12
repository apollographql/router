### Additional Connector Custom Instrument Selectors ([PR #8045](https://github.com/apollographql/router/pull/8045))

This adds new [custom instrument selectors](https://www.apollographql.com/docs/graphos/routing/observability/telemetry/instrumentation/selectors#connector) for Connectors and enhances some existing selectors. The new selectors are:
 - `supergraph_operation_name`
   - The supergraph's operation name
 - `supergraph_operation_kind`
   - The supergraph's operation type (e.g. `query`, `mutation`, `subscription`)
 - `request_context`
   - Takes the value of the given key on the request context
 - `connector_on_response_error`
   - Returns true when the response does not meet the `is_successful` condition. Or, if that condition is not set,
     returns true when the response has a non-200 status code

These selectors were modified to add additional functionality:
 - `connector_request_mapping_problems`
   - Adds a new `boolean` variant that will return `true` when a mapping problem exists on the request
 - `connector_response_mapping_problems`
   - Adds a new `boolean` variant that will return `true` when a mapping problem exists on the response

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/8045