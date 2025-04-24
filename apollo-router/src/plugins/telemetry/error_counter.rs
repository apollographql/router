use std::collections::HashMap;
use std::sync::Arc;
use serde_json_bytes::Value;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::{OPERATION_KIND, OPERATION_NAME};
use crate::{graphql, Context};
use crate::plugins::telemetry::apollo::{ErrorsConfiguration, ExtendedErrorMetricsMode};
use crate::plugins::telemetry::{CLIENT_NAME, CLIENT_VERSION};
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::{SupergraphResponse};
use crate::services::router::ClientRequestAccepts;
use crate::spec::query::EXTENSIONS_VALUE_COMPLETION_KEY;

// TODO migrate subgraph and extended errors config
pub(crate) async fn count_errors(mut response: SupergraphResponse, errors_config: &ErrorsConfiguration) {
    let context = response.context.clone();
    // TODO do we really need this? 
    let ClientRequestAccepts {
        wildcard: accepts_wildcard,
        json: accepts_json,
        multipart_defer: accepts_multipart_defer,
        multipart_subscription: accepts_multipart_subscription,
    } = context
        .extensions()
        .with_lock(|lock| lock.get().cloned())
        .unwrap_or_default();


    if let Some(gql_response) = response.next_response().await {
        if !gql_response.has_next.unwrap_or(false)
            && !gql_response.subscribed.unwrap_or(false)
            && (accepts_json || accepts_wildcard)
        {
            if !gql_response.errors.is_empty() {
                count_operation_errors(&gql_response.errors, &context, &errors_config);
            }
            if let Some(value_completion) = gql_response.extensions.get(EXTENSIONS_VALUE_COMPLETION_KEY) {
                // TODO inline this func?
                count_value_completion_errors(
                    value_completion,
                    &context,
                    &errors_config,
                );
            }
        } else if accepts_multipart_defer || accepts_multipart_subscription {
            // TODO can we combine this with above?
            if !gql_response.errors.is_empty() {
                count_operation_errors(&gql_response.errors, &context, &errors_config);
            }
        } else {
            // TODO supposedly this is unreachable in router service. Will we be able to pick this up in a router service plugin callback instead?
            // TODO I'm guessing no b/c at the plugin layer, we'd have to parse the response as json.
            // TODO As is, this feels really bad b/c the error will be defined _AFTER_ we count it in router/service.rs
            count_operation_error_codes(
                &["INVALID_ACCEPT_HEADER"],
                &context,
                &errors_config,
            );
        }
    }
    
    // TODO router service plugin fn to capture SERVICE_UNAVAILABLE or INVALID_ACCEPT_HEADER? Would need to parse json response
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

fn count_value_completion_errors(
    value_completion: &Value,
    context: &Context,
    errors_config: &ErrorsConfiguration,
) {
    if let Some(vc_array) = value_completion.as_array() {
        let errors: Vec<graphql::Error> = vc_array
            .iter()
            .filter_map(graphql::Error::from_value_completion_value)
            .collect();
        crate::metrics::count_operation_errors(&errors, context, errors_config);
    }
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

    let maybe_usage_reporting = context
        .extensions()
        .with_lock(|lock| lock.get::<Arc<UsageReporting>>().cloned());

    if let Some(usage_reporting) = maybe_usage_reporting {
        // Try to get operation ID from usage reporting if it's not in context (e.g. on parse/validation error)
        if operation_id.is_empty() {
            operation_id = usage_reporting.get_operation_id();
        }

        // Also try to get operation name from usage reporting if it's not in context
        if operation_name.is_empty() {
            operation_name = usage_reporting.get_operation_name();
        }
    }

    let mut map = HashMap::new();
    for error in errors {
        let code = error.extensions.get("code").and_then(|c| match c {
            Value::String(s) => Some(s.as_str().to_owned()),
            Value::Bool(b) => Some(format!("{b}")),
            Value::Number(n) => Some(n.to_string()),
            Value::Null | Value::Array(_) | Value::Object(_) => None,
        });
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
        let entry = map.entry(code.clone()).or_insert(0u64);
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
                "graphql.error.extensions.code" = code.unwrap_or_default(),
                "graphql.error.extensions.severity" = severity_str,
                "graphql.error.path" = path,
                "apollo.router.error.service" = service
            );
        }
    }

    for (code, count) in map {
        count_graphql_error(count, code.as_deref());
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