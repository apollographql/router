### Remove deprecated 1.x context key aliases for coprocessors ([PR #9686](https://github.com/apollographql/router/pull/9686))

The `context: deprecated` coprocessor configuration option has been removed. It was a compatibility
layer that translated context keys between their Router 1.x names (e.g. `operation_name`) and their
current names (e.g. `apollo::supergraph::operation_name`) when sending and receiving context from a
coprocessor. Deprecation warnings were emitted in Router 2.x.

Additionally, the telemetry plugin no longer falls back to reading `apollo_telemetry::client_name`
and `apollo_telemetry::client_version` from context. These keys were the 1.x names for
`apollo::telemetry::client_name` and `apollo::telemetry::client_version`. If you have any
Rhai scripts or custom plugins that write client name or version using the old key names, update
them to use the current names; otherwise client attribution in Studio will stop working.

If you have `context: deprecated` in your coprocessor config, update to `context: all` (or a
selective list of key names) and update your coprocessor to reference current context key names:

| Old 1.x name | Current name |
|---|---|
| `operation_name` | `apollo::supergraph::operation_name` |
| `operation_kind` | `apollo::supergraph::operation_kind` |
| `apollo_authentication::JWT::claims` | `apollo::authentication::jwt_claims` |
| `apollo_authorization::authenticated::required` | `apollo::authorization::authentication_required` |
| `apollo_authorization::scopes::required` | `apollo::authorization::required_scopes` |
| `apollo_authorization::policies::required` | `apollo::authorization::required_policies` |
| `cost.estimated` | `apollo::demand_control::estimated_cost` |
| `cost.actual` | `apollo::demand_control::actual_cost` |
| `cost.result` | `apollo::demand_control::result` |
| `cost.strategy` | `apollo::demand_control::strategy` |
| `experimental::expose_query_plan.plan` | `apollo::expose_query_plan::plan` |
| `experimental::expose_query_plan.formatted_plan` | `apollo::expose_query_plan::formatted_plan` |
| `experimental::expose_query_plan.enabled` | `apollo::expose_query_plan::enabled` |
| `apollo_override::unresolved_labels` | `apollo::progressive_override::unresolved_labels` |
| `apollo_override::labels_to_override` | `apollo::progressive_override::labels_to_override` |
| `apollo_telemetry::client_name` | `apollo::telemetry::client_name` |
| `apollo_telemetry::client_version` | `apollo::telemetry::client_version` |
| `apollo_telemetry::subgraph_ftv1` | `apollo::telemetry::subgraph_ftv1` |
| `apollo_telemetry::studio::exclude` | `apollo::telemetry::studio_exclude` |
| `apollo_operation_id` | `apollo::supergraph::operation_id` |
| `apollo_router::supergraph::first_event` | `apollo::supergraph::first_event` |
| `persisted_query_hit` | `apollo::apq::cache_hit` |
| `persisted_query_register` | `apollo::apq::registered` |

Config migration is automatic: `context: deprecated` is migrated to `context: all`.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9686
