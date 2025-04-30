use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::ready;
use futures::stream::once;
use serde::de::DeserializeOwned;
use serde_json_bytes::Value;

use crate::Context;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::COUNTED_ERRORS;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::graphql;
use crate::graphql::Error;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::apollo::ErrorsConfiguration;
use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::{router, ExecutionResponse, RouterResponse};
use crate::services::SubgraphResponse;
use crate::services::SupergraphResponse;
use crate::plugins::content_negotiation::ClientRequestAccepts;
use crate::spec::query::EXTENSIONS_VALUE_COMPLETION_KEY;

pub(crate) async fn count_subgraph_errors(
    response: SubgraphResponse,
    errors_config: &ErrorsConfiguration,
) -> SubgraphResponse {
    let context = response.context.clone();
    let errors_config = errors_config.clone();

    let response_body = response.response.body();
    if !response_body.errors.is_empty() {
        count_operation_errors(&response_body.errors, &context, &errors_config);
    }
    context
        .insert(COUNTED_ERRORS, to_map(&response_body.errors))
        .expect("Unable to insert errors into context.");

    SubgraphResponse {
        context: response.context,
        subgraph_name: response.subgraph_name,
        id: response.id,
        response: response.response,
    }
}

pub(crate) async fn count_supergraph_errors(
    response: SupergraphResponse,
    errors_config: &ErrorsConfiguration,
) -> SupergraphResponse {
    // TODO streaming subscriptions?
    // TODO multiple responses in the stream?

    let context = response.context.clone();
    let errors_config = errors_config.clone();

    let (parts, stream) = response.response.into_parts();

    let stream = stream.inspect(move |response_body| {
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

        if !response_body.has_next.unwrap_or(false)
            && !response_body.subscribed.unwrap_or(false)
            && (accepts_json || accepts_wildcard)
        {
            // TODO ensure free plan is captured
            if !response_body.errors.is_empty() {
                count_operation_errors(&response_body.errors, &context, &errors_config);
            }
            if let Some(value_completion) = response_body
                .extensions
                .get(EXTENSIONS_VALUE_COMPLETION_KEY)
            {
                // TODO inline this func?
                count_value_completion_errors(value_completion, &context, &errors_config);
            }
        } else if accepts_multipart_defer || accepts_multipart_subscription {
            // TODO can we combine this with above?
            if !response_body.errors.is_empty() {
                count_operation_errors(&response_body.errors, &context, &errors_config);
            }
        } else {
            // TODO supposedly this is unreachable in router service. Will we be able to pick this up in a router service plugin callback instead?
            // TODO I'm guessing no b/c at the plugin layer, we'd have to parse the response as json.
            // TODO As is, this feels really bad b/c the error will be defined _AFTER_ we count it in router/service.rs
            count_operation_error_codes(&["INVALID_ACCEPT_HEADER"], &context, &errors_config);
        }

        // Refresh context with the most up-to-date list of errors
        context
            .insert(COUNTED_ERRORS, to_map(&response_body.errors))
            .expect("Unable to insert errors into context.");
    });

    let (first_response, rest) = StreamExt::into_future(stream).await;
    let new_response = http::Response::from_parts(
        parts,
        once(ready(first_response.unwrap_or_default()))
            .chain(rest)
            .boxed(),
    );

    SupergraphResponse {
        context: response.context,
        response: new_response,
    }
}

pub(crate) async fn count_execution_errors(
    response: ExecutionResponse,
    errors_config: &ErrorsConfiguration,
) -> ExecutionResponse {
    let context = response.context.clone();
    let errors_config = errors_config.clone();

    let (parts, stream) = response.response.into_parts();

    let stream = stream.inspect(move |response_body| {
        if !response_body.errors.is_empty() {
            count_operation_errors(&response_body.errors, &context, &errors_config);
        }
        context
            .insert(COUNTED_ERRORS, to_map(&response_body.errors))
            .expect("Unable to insert errors into context.");
    });

    let (first_response, rest) = StreamExt::into_future(stream).await;
    let new_response = http::Response::from_parts(
        parts,
        once(ready(first_response.unwrap_or_default()))
            .chain(rest)
            .boxed(),
    );

    ExecutionResponse {
        context: response.context,
        response: new_response,
    }
}

pub(crate) async fn count_router_errors(
    response: RouterResponse,
    errors_config: &ErrorsConfiguration,
) -> RouterResponse {
    let context = response.context.clone();
    let errors_config = errors_config.clone();

    let (parts, body) = response.response.into_parts();

    // TODO is this a bad idea? Probably...
    // Deserialize the response body back into a response obj so we can pull the errors
    let bytes = router::body::into_bytes(body)
        .await
        .unwrap();
    let response_body: graphql::Response = serde_json::from_slice(&bytes).unwrap();

    if !response_body.errors.is_empty() {
        count_operation_errors(&response_body.errors, &context, &errors_config);
    }

    // Refresh context with the most up-to-date list of errors
    context
        .insert(COUNTED_ERRORS, to_map(&response_body.errors))
        .expect("Unable to insert errors into context.");

    RouterResponse {
        context: response.context,
        response: http::Response::from_parts(parts, router::body::from_bytes(bytes)),
    }
}

// TODO how do we parse the json response to capture SERVICE_UNAVAILABLE or INVALID_ACCEPT_HEADER in a count_router_errors()?

fn to_map(errors: &[Error]) -> HashMap<String, u64> {
    let mut map: HashMap<String, u64> = HashMap::new();
    errors.iter().for_each(|error| {
        // TODO hash the full error more uniquely
        map.entry(get_code(error).unwrap_or_default())
            .and_modify(|count| *count += 1)
            .or_insert(1);
    });

    map
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
        count_operation_errors(&errors, context, errors_config);
    }
}

fn count_operation_errors(
    errors: &[Error],
    context: &Context,
    errors_config: &ErrorsConfiguration,
) {
    let previously_counted_errors_map: HashMap<String, u64> =
        unwrap_from_context(context, COUNTED_ERRORS);

    let mut operation_id: String = unwrap_from_context(context, APOLLO_OPERATION_ID);
    let mut operation_name: String = unwrap_from_context(context, OPERATION_NAME);
    let operation_kind: String = unwrap_from_context(context, OPERATION_KIND);
    let client_name: String = unwrap_from_context(context, CLIENT_NAME);
    let client_version: String = unwrap_from_context(context, CLIENT_VERSION);

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

    // TODO how do we account for redacted errors when comparing? Likely skip them completely (they will have been counted with correct codes in subgraph layer)
    let mut diff_map = previously_counted_errors_map.clone();
    for error in errors {
        let code = get_code(error).unwrap_or_default();

        // If we already counted this error in a previous layer, then skip counting it again
        if let Some(count) = diff_map.get_mut(&code) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                diff_map.remove(&code);
            }
            continue;
        }

        // If we haven't seen this error before, or we see more occurrences than we've counted
        // before, then count the error
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
                "graphql.error.extensions.code" = code.clone(),
                "graphql.error.extensions.severity" = severity_str,
                "graphql.error.path" = path,
                "apollo.router.error.service" = service
            );
        }
        count_graphql_error(1, code);
    }
}

fn unwrap_from_context<V: Default + DeserializeOwned>(context: &Context, key: &str) -> V {
    context
        .get::<_, V>(key) // -> Option<Result<T, E>>
        .unwrap_or_default() // -> Result<T, E> (defaults to Ok(T::default()))
        .unwrap_or_default() // -> T (defaults on Err)
}

fn get_code(error: &Error) -> Option<String> {
    error.extensions.get("code").and_then(|c| match c {
        Value::String(s) => Some(s.as_str().to_owned()),
        Value::Bool(b) => Some(format!("{b}")),
        Value::Number(n) => Some(n.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    })
}

fn count_graphql_error(count: u64, code: String) {
    // TODO ensure an empty string matches when we used a None optional before
    u64_counter!(
        "apollo.router.graphql_error",
        "Number of GraphQL error responses returned by the router",
        count,
        code = code
    );
}

#[cfg(test)]
mod test {
    use http::StatusCode;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;

    use crate::Context;
    use crate::context::COUNTED_ERRORS;
    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::json_ext::Path;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::CLIENT_NAME;
    use crate::plugins::telemetry::CLIENT_VERSION;
    use crate::plugins::telemetry::apollo::ErrorsConfiguration;
    use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
    use crate::plugins::telemetry::error_counter::count_operation_error_codes;
    use crate::plugins::telemetry::error_counter::count_operation_errors;
    use crate::plugins::telemetry::error_counter::count_supergraph_errors;
    use crate::query_planner::APOLLO_OPERATION_ID;
    use crate::services::SupergraphResponse;
    use crate::plugins::content_negotiation::ClientRequestAccepts;

    #[tokio::test]
    async fn test_count_errors_with_no_previously_counted_errors() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();

            context.extensions().with_lock(|lock| {
                lock.insert(ClientRequestAccepts {
                    multipart_defer: false,
                    multipart_subscription: false,
                    json: true,
                    wildcard: false,
                })
            });

            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            let new_response = count_supergraph_errors(
                SupergraphResponse::fake_builder()
                    .header("Accept", "application/json")
                    .context(context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .errors(vec![
                        graphql::Error::builder()
                            .message("You did a bad request.")
                            .extension_code("GRAPHQL_VALIDATION_FAILED")
                            .build(),
                    ])
                    .build()
                    .unwrap(),
                &config,
            )
            .await;

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
                "apollo.router.graphql_error",
                1,
                code = "GRAPHQL_VALIDATION_FAILED"
            );

            assert_eq!(
                new_response.context.get_json_value(COUNTED_ERRORS),
                Some(json!({"GRAPHQL_VALIDATION_FAILED": 1}))
            )
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_count_errors_with_previously_counted_errors() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();

            context.extensions().with_lock(|lock| {
                lock.insert(ClientRequestAccepts {
                    multipart_defer: false,
                    multipart_subscription: false,
                    json: true,
                    wildcard: false,
                })
            });

            let _ = context.insert(COUNTED_ERRORS, json!({"GRAPHQL_VALIDATION_FAILED": 1}));

            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            let new_response = count_supergraph_errors(
                SupergraphResponse::fake_builder()
                    .header("Accept", "application/json")
                    .context(context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .error(
                        graphql::Error::builder()
                            .message("You did a bad request.")
                            .extension_code("GRAPHQL_VALIDATION_FAILED")
                            .build(),
                    )
                    .error(
                        graphql::Error::builder()
                            .message("Custom error text")
                            .extension_code("CUSTOM_ERROR")
                            .build(),
                    )
                    .build()
                    .unwrap(),
                &config,
            )
            .await;

            assert_counter!(
                "apollo.router.operations.error",
                1,
                "apollo.operation.id" = "some-id",
                "graphql.operation.name" = "SomeOperation",
                "graphql.operation.type" = "query",
                "apollo.client.name" = "client-1",
                "apollo.client.version" = "version-1",
                "graphql.error.extensions.code" = "CUSTOM_ERROR",
                "graphql.error.extensions.severity" = "ERROR",
                "graphql.error.path" = "",
                "apollo.router.error.service" = ""
            );

            assert_counter!("apollo.router.graphql_error", 1, code = "CUSTOM_ERROR");

            assert_counter_not_exists!(
                "apollo.router.operations.error",
                u64,
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

            assert_counter_not_exists!(
                "apollo.router.graphql_error",
                u64,
                code = "GRAPHQL_VALIDATION_FAILED"
            );

            assert_eq!(
                new_response.context.get_json_value(COUNTED_ERRORS),
                Some(json!({"GRAPHQL_VALIDATION_FAILED": 1, "CUSTOM_ERROR": 1}))
            )
        }
        .with_metrics()
        .await;
    }

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
