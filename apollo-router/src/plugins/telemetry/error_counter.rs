use std::collections::HashMap;
use std::sync::Arc;
use serde_json_bytes::{Value};
use crate::apollo_studio_interop::UsageReporting;
use crate::context::{OPERATION_KIND, OPERATION_NAME};
use crate::{graphql, Context};
use crate::plugins::telemetry::apollo::{ErrorsConfiguration, ExtendedErrorMetricsMode};
use crate::plugins::telemetry::{CLIENT_NAME, CLIENT_VERSION};
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::{SupergraphResponse};
use crate::services::router::ClientRequestAccepts;
use crate::spec::query::EXTENSIONS_VALUE_COMPLETION_KEY;

// TODO call this for subgraph service (pre redaction), supergraph service, and _MAYBE_ router service (service unavail and invalid headers)
pub(crate) async fn count_errors(mut response: SupergraphResponse, errors_config: &ErrorsConfiguration) -> SupergraphResponse {
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
        // TODO make mapping to add to response context to avoid double counting

        if !gql_response.has_next.unwrap_or(false)
            && !gql_response.subscribed.unwrap_or(false)
            && (accepts_json || accepts_wildcard)
        {
            // TODO ensure free plan is captured
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
    response
}

// TODO router service plugin fn to capture SERVICE_UNAVAILABLE or INVALID_ACCEPT_HEADER? Would need to parse json response

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
        count_operation_errors(&errors, context, errors_config);
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


#[cfg(test)]
mod test {
    use serde_json_bytes::Value;
    use serde_json_bytes::json;

    use crate::Context;
    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::json_ext::Path;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::CLIENT_NAME;
    use crate::plugins::telemetry::CLIENT_VERSION;
    use crate::plugins::telemetry::apollo::ErrorsConfiguration;
    use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
    use crate::plugins::telemetry::error_counter::{count_operation_error_codes, count_operation_errors};
    use crate::query_planner::APOLLO_OPERATION_ID;

    #[tokio::test]
    async fn test_count_operation_error_codes_with_extended_config_enabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();
            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            count_operation_error_codes(
                &["GRAPHQL_VALIDATION_FAILED", "MY_CUSTOM_ERROR", "400"],
                &context,
                &config,
            );

            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "",
                    "apollo.router.error.service" = ""
                );
            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "MY_CUSTOM_ERROR",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "",
                    "apollo.router.error.service" = ""
                );

            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "400",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "",
                    "apollo.router.error.service" = ""
                );

            assert_counter!(
                    "apollo.router.graphql_error",
                    1,
                    code = "GRAPHQL_VALIDATION_FAILED"
                );
            assert_counter!("apollo.router.graphql_error", 1, code = "MY_CUSTOM_ERROR");
            assert_counter!("apollo.router.graphql_error", 1, code = "400");
        }
            .with_metrics()
            .await;
    }

    #[tokio::test]
    async fn test_count_operation_error_codes_with_extended_config_disabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Disabled,
                ..Default::default()
            };

            let context = Context::default();
            count_operation_error_codes(
                &["GRAPHQL_VALIDATION_FAILED", "MY_CUSTOM_ERROR", "400"],
                &context,
                &config,
            );

            assert_counter_not_exists!(
                    "apollo.router.operations.error",
                    u64,
                    "apollo.operation.id" = "",
                    "graphql.operation.name" = "",
                    "graphql.operation.type" = "",
                    "apollo.client.name" = "",
                    "apollo.client.version" = "",
                    "graphql.error.extensions.code" = "GRAPHQL_VALIDATION_FAILED",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "",
                    "apollo.router.error.service" = ""
                );
            assert_counter_not_exists!(
                    "apollo.router.operations.error",
                    u64,
                    "apollo.operation.id" = "",
                    "graphql.operation.name" = "",
                    "graphql.operation.type" = "",
                    "apollo.client.name" = "",
                    "apollo.client.version" = "",
                    "graphql.error.extensions.code" = "MY_CUSTOM_ERROR",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "",
                    "apollo.router.error.service" = ""
                );
            assert_counter_not_exists!(
                    "apollo.router.operations.error",
                    u64,
                    "apollo.operation.id" = "",
                    "graphql.operation.name" = "",
                    "graphql.operation.type" = "",
                    "apollo.client.name" = "",
                    "apollo.client.version" = "",
                    "graphql.error.extensions.code" = "400",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "",
                    "apollo.router.error.service" = ""
                );

            assert_counter!(
                    "apollo.router.graphql_error",
                    1,
                    code = "GRAPHQL_VALIDATION_FAILED"
                );
            assert_counter!("apollo.router.graphql_error", 1, code = "MY_CUSTOM_ERROR");
            assert_counter!("apollo.router.graphql_error", 1, code = "400");
        }
            .with_metrics()
            .await;
    }

    #[tokio::test]
    async fn test_count_operation_errors_with_extended_config_enabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();
            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            let error = graphql::Error::builder()
                .message("some error")
                .extension_code("SOME_ERROR_CODE")
                .extension("service", "mySubgraph")
                .path(Path::from("obj/field"))
                .build();

            count_operation_errors(&[error], &context, &config);

            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "SOME_ERROR_CODE",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 1, code = "SOME_ERROR_CODE");
        }
            .with_metrics()
            .await;
    }

    #[tokio::test]
    async fn test_count_operation_errors_with_all_json_types_and_extended_config_enabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();
            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            let codes = [
                json!("VALID_ERROR_CODE"),
                json!(400),
                json!(true),
                Value::Null,
                json!(["code1", "code2"]),
                json!({"inner": "myCode"}),
            ];

            let errors = codes.map(|code| {
                graphql::Error::from_value(json!(
                    {
                      "message": "error occurred",
                      "extensions": {
                        "code": code,
                        "service": "mySubgraph"
                      },
                      "path": ["obj", "field"]
                    }
                    ))
                    .unwrap()
            });

            count_operation_errors(&errors, &context, &config);

            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "VALID_ERROR_CODE",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 1, code = "VALID_ERROR_CODE");

            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "400",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 1, code = "400");

            // Code is ignored for null, arrays, and objects

            assert_counter!(
                    "apollo.router.operations.error",
                    1,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "true",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 1, code = "true");

            assert_counter!(
                    "apollo.router.operations.error",
                    3,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 3);
        }
            .with_metrics()
            .await;
    }

    #[tokio::test]
    async fn test_count_operation_errors_with_duplicate_errors_and_extended_config_enabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();
            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            let codes = [
                json!("VALID_ERROR_CODE"),
                Value::Null,
                json!("VALID_ERROR_CODE"),
                Value::Null,
            ];

            let errors = codes.map(|code| {
                graphql::Error::from_value(json!(
                    {
                      "message": "error occurred",
                      "extensions": {
                        "code": code,
                        "service": "mySubgraph"
                      },
                      "path": ["obj", "field"]
                    }
                    ))
                    .unwrap()
            });

            count_operation_errors(&errors, &context, &config);

            assert_counter!(
                    "apollo.router.operations.error",
                    2,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "VALID_ERROR_CODE",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 2, code = "VALID_ERROR_CODE");

            assert_counter!(
                    "apollo.router.operations.error",
                    2,
                    "apollo.operation.id" = "some-id",
                    "graphql.operation.name" = "SomeOperation",
                    "graphql.operation.type" = "query",
                    "apollo.client.name" = "client-1",
                    "apollo.client.version" = "version-1",
                    "graphql.error.extensions.code" = "",
                    "graphql.error.extensions.severity" = "ERROR",
                    "graphql.error.path" = "/obj/field",
                    "apollo.router.error.service" = "mySubgraph"
                );

            assert_counter!("apollo.router.graphql_error", 2);
        }
            .with_metrics()
            .await;
    }
}