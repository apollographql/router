use std::sync::Arc;
use std::collections::HashMap;

use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
use crate::Context;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::spec::GRAPHQL_PARSE_FAILURE_ERROR_KEY;
use crate::spec::GRAPHQL_UNKNOWN_OPERATION_NAME_ERROR_KEY;
use crate::spec::GRAPHQL_VALIDATION_FAILURE_ERROR_KEY;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use crate::query_planner::stats_report_key_hash;
use tower::ServiceExt;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::SubgraphResponse;
use crate::services::subgraph;
use crate::graphql;


use super::telemetry::apollo::ErrorsConfiguration;

static REDACTED_ERROR_MESSAGE: &str = "Subgraph errors redacted";

register_plugin!("apollo", "error_counting_metrics", ErrorCountingMetrics);

/// Configuration for exposing errors that originate from subgraphs
#[derive(Clone, Debug, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct Config {
    // TODO
}

struct ErrorCountingMetrics {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for ErrorCountingMetrics {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(ErrorCountingMetrics {
            config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        // Search for subgraph in our configured subgraph map. If we can't find it, use the "all" value

        service
            .map_response(move |mut response: SubgraphResponse| {
                let errors = &mut response.response.body_mut().errors;
                if !errors.is_empty() {
                    count_operation_errors(
                        &errors,
                        &response.context,
                        &self.apollo_telemetry_config.errors,
                    );
                }
                // TODO value completion errors?

                // TODO count_operation_error_codes() invalid accept header case? May be impossible
                // due to needing to remake the if/elseif or at minimum duplicating logic

                // We don't need to bother with `count_graphql_error()` call for free 
                // tier rate limiting b/c it doesn't emit a metric with context
                // It will be called by `count_operation_errors()` though
                response
            }) // TODO use map_err?
            .boxed()
    }

// TODO execution_service for connectors errors?
}

fn count_operation_error_codes(
    codes: &[&str],
    context: &Context,
    errors_config: &ErrorsConfiguration,
) {
    let errors: Vec<graphql::Error> = codes
        .iter()
        .map(|c| {
            graphql::Error::builder()
                .message("")
                .extension_code(*c)
                .build()
        })
        .collect();

    count_operation_errors(&errors, context, errors_config);
}

fn count_operation_errors(
    errors: &[graphql::Error],
    context: &Context,
    errors_config: &ErrorsConfiguration,
) {
    let unwrap_context_string = |context_key: &str| -> String {
        context
            .get::<_, String>(context_key)
            .unwrap_or_default()
            .unwrap_or_default()
    };

    let mut operation_id = unwrap_context_string(APOLLO_OPERATION_ID);
    let mut operation_name = unwrap_context_string(OPERATION_NAME);
    let operation_kind = unwrap_context_string(OPERATION_KIND);
    let client_name = unwrap_context_string(CLIENT_NAME);
    let client_version = unwrap_context_string(CLIENT_VERSION);

    // Try to get operation ID from the stats report key if it's not in context (e.g. on parse/validation error)
    if operation_id.is_empty() {
        let maybe_stats_report_key = context.extensions().with_lock(|lock| {
            lock.get::<Arc<UsageReporting>>()
                .map(|u| u.stats_report_key.clone())
        });
        if let Some(stats_report_key) = maybe_stats_report_key {
            operation_id = stats_report_key_hash(stats_report_key.as_str());

            // If the operation name is empty, it's possible it's an error and we can populate the name by skipping the
            // first character of the stats report key ("#") and the last newline character. E.g.
            // "## GraphQLParseFailure\n" will turn into "# GraphQLParseFailure".
            if operation_name.is_empty() {
                operation_name = match stats_report_key.as_str() {
                    GRAPHQL_PARSE_FAILURE_ERROR_KEY
                    | GRAPHQL_UNKNOWN_OPERATION_NAME_ERROR_KEY
                    | GRAPHQL_VALIDATION_FAILURE_ERROR_KEY => stats_report_key
                        .chars()
                        .skip(1)
                        .take(stats_report_key.len() - 2)
                        .collect(),
                    _ => "".to_string(),
                }
            }
        }
    }

    let mut map = HashMap::new();
    for error in errors {
        let code = error.extensions.get("code").and_then(|c| c.as_str());
        let service = error
            .extensions
            .get("service")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let severity = error.extensions.get("severity").and_then(|s| s.as_str());
        let path = match &error.path {
            None => "".into(),
            Some(path) => path.to_string(),
        };
        let entry = map.entry(code).or_insert(0u64);
        *entry += 1;

        let send_otlp_errors = if service.is_empty() {
            matches!(
                errors_config.preview_extended_error_metrics,
                ExtendedErrorMetricsMode::Enabled
            )
        } else {
            let subgraph_error_config = errors_config.subgraph.get_error_config(&service);
            subgraph_error_config.send
                && matches!(
                    errors_config.preview_extended_error_metrics,
                    ExtendedErrorMetricsMode::Enabled
                )
        };

        if send_otlp_errors {
            let code_str = code.unwrap_or_default().to_string();
            let severity_str = severity
                .unwrap_or(tracing::Level::ERROR.as_str())
                .to_string();
            u64_counter!(
                "apollo.router.operations.error",
                "Number of errors returned by operation",
                1,
                "apollo.operation.id" = operation_id.clone(),
                "graphql.operation.name" = operation_name.clone(),
                "graphql.operation.type" = operation_kind.clone(),
                "apollo.client.name" = client_name.clone(),
                "apollo.client.version" = client_version.clone(),
                "graphql.error.extensions.code" = code_str,
                "graphql.error.extensions.severity" = severity_str,
                "graphql.error.path" = path,
                "apollo.router.error.service" = service
            );
        }
    }

    for (code, count) in map {
        count_graphql_error(count, code);
    }
}

/// Shared counter for `apollo.router.graphql_error` for consistency
fn count_graphql_error(count: u64, code: Option<&str>) {
    match code {
        None => {
            u64_counter!(
                "apollo.router.graphql_error",
                "Number of GraphQL error responses returned by the router",
                count
            );
        }
        Some(code) => {
            u64_counter!(
                "apollo.router.graphql_error",
                "Number of GraphQL error responses returned by the router",
                count,
                code = code.to_string()
            );
        }
    }
}

#[cfg(test)]
mod test {}
