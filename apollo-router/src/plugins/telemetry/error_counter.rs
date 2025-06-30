use std::sync::Arc;

use ahash::HashMap;
use ahash::HashSet;
use futures::StreamExt;
use futures::future::ready;
use futures::stream::once;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::Context;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::COUNTED_ERRORS;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::context::ROUTER_RESPONSE_ERRORS;
use crate::graphql;
use crate::graphql::Error;
use crate::plugins::telemetry::CLIENT_NAME;
use crate::plugins::telemetry::CLIENT_VERSION;
use crate::plugins::telemetry::apollo::ErrorsConfiguration;
use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
use crate::query_planner::APOLLO_OPERATION_ID;
use crate::services::ExecutionResponse;
use crate::services::RouterResponse;
use crate::services::SubgraphResponse;
use crate::services::SupergraphResponse;
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
        // Refresh context with the most up-to-date list of errors
        let _ = context.insert(COUNTED_ERRORS, to_set(&response_body.errors));
    }
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
        // TODO ensure free plan is captured
        if !response_body.errors.is_empty() {
            count_operation_errors(&response_body.errors, &context, &errors_config);
        }
        if let Some(value_completion) = response_body
            .extensions
            .get(EXTENSIONS_VALUE_COMPLETION_KEY)
        {
            if let Some(vc_array) = value_completion.as_array() {
                // We only count these in the supergraph layer to avoid double counting
                let errors: Vec<graphql::Error> = vc_array
                    .iter()
                    .filter_map(graphql::Error::from_value_completion_value)
                    .collect();
                count_operation_errors(&errors, &context, &errors_config);
            }
        }

        // Refresh context with the most up-to-date list of errors
        let _ = context.insert(COUNTED_ERRORS, to_set(&response_body.errors));
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
            // Refresh context with the most up-to-date list of errors
            let _ = context.insert(COUNTED_ERRORS, to_set(&response_body.errors));
        }
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

    // We look at context for our current errors instead of the existing response to avoid a full
    // response deserialization.
    let errors_by_id: HashMap<Uuid, Error> = unwrap_from_context(&context, ROUTER_RESPONSE_ERRORS);
    let errors: Vec<Error> = errors_by_id
        .iter()
        .map(|(id, error)| error.with_apollo_id(*id))
        .collect();
    if !errors.is_empty() {
        count_operation_errors(&errors, &context, &errors_config);
        // Router layer handling is unique in that the list of new errors from context may not
        // include errors we previously counted. Thus, we must combine the set of previously counted
        // errors with the set of new errors here before adding to context.
        let mut counted_errors: HashSet<Uuid> = unwrap_from_context(&context, COUNTED_ERRORS);
        counted_errors.extend(errors.iter().map(Error::apollo_id));
        let _ = context.insert(COUNTED_ERRORS, counted_errors);
    }

    RouterResponse {
        context: response.context,
        response: response.response,
    }
}

fn to_set(errors: &[Error]) -> HashSet<Uuid> {
    errors.iter().map(Error::apollo_id).collect()
}

fn count_operation_errors(
    errors: &[Error],
    context: &Context,
    errors_config: &ErrorsConfiguration,
) {
    let previously_counted_errors_map: HashSet<Uuid> = unwrap_from_context(context, COUNTED_ERRORS);

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
    // TODO ^This might not matter now that we're using apollo_id
    for error in errors {
        let apollo_id = error.apollo_id();

        // If we already counted this error in a previous layer, then skip counting it again
        if previously_counted_errors_map.contains(&apollo_id) {
            continue;
        }

        // If we haven't seen this error before, then count it
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

        let code = error.extension_code().unwrap_or_default();

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
        .get::<_, V>(key)
        .unwrap_or_default()
        .unwrap_or_default()
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
    use std::collections::HashMap;
    use std::collections::HashSet;

    use http::StatusCode;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;
    use uuid::Uuid;

    use crate::Context;
    use crate::context::COUNTED_ERRORS;
    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::json_ext::Path;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::CLIENT_NAME;
    use crate::plugins::telemetry::CLIENT_VERSION;
    use crate::plugins::telemetry::apollo::ErrorConfiguration;
    use crate::plugins::telemetry::apollo::ErrorRedactionPolicy;
    use crate::plugins::telemetry::apollo::ErrorsConfiguration;
    use crate::plugins::telemetry::apollo::ExtendedErrorMetricsMode;
    use crate::plugins::telemetry::apollo::SubgraphErrorConfig;
    use crate::plugins::telemetry::error_counter::count_operation_errors;
    use crate::plugins::telemetry::error_counter::count_subgraph_errors;
    use crate::plugins::telemetry::error_counter::count_supergraph_errors;
    use crate::plugins::telemetry::error_counter::unwrap_from_context;
    use crate::query_planner::APOLLO_OPERATION_ID;
    use crate::services::SubgraphResponse;
    use crate::services::SupergraphResponse;

    #[tokio::test]
    async fn test_count_supergraph_errors_with_no_previously_counted_errors() {
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

            let error_id = Uuid::new_v4();
            let new_response = count_supergraph_errors(
                SupergraphResponse::fake_builder()
                    .header("Accept", "application/json")
                    .context(context)
                    .status_code(StatusCode::BAD_REQUEST)
                    .errors(vec![
                        graphql::Error::builder()
                            .message("You did a bad request.")
                            .extension_code("GRAPHQL_VALIDATION_FAILED")
                            .apollo_id(error_id)
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
                unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
                HashSet::from([error_id])
            )
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_count_supergraph_errors_with_previously_counted_errors() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                ..Default::default()
            };

            let context = Context::default();
            let validation_error_id = Uuid::new_v4();
            let custom_error_id = Uuid::new_v4();

            let _ = context.insert(COUNTED_ERRORS, HashSet::from([validation_error_id]));

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
                            .apollo_id(validation_error_id)
                            .build(),
                    )
                    .error(
                        graphql::Error::builder()
                            .message("Custom error text")
                            .extension_code("CUSTOM_ERROR")
                            .apollo_id(custom_error_id)
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
                unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
                HashSet::from([validation_error_id, custom_error_id])
            )
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_count_subgraph_errors_with_include_subgraphs_enabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                subgraph: SubgraphErrorConfig {
                    subgraphs: HashMap::from([(
                        "some-subgraph".to_string(),
                        ErrorConfiguration {
                            send: true,
                            redact: false,
                            redaction_policy: ErrorRedactionPolicy::Strict,
                        },
                    )]),
                    ..Default::default()
                },
                ..Default::default()
            };

            let context = Context::default();
            let _ = context.insert(APOLLO_OPERATION_ID, "some-id".to_string());
            let _ = context.insert(OPERATION_NAME, "SomeOperation".to_string());
            let _ = context.insert(OPERATION_KIND, "query".to_string());
            let _ = context.insert(CLIENT_NAME, "client-1".to_string());
            let _ = context.insert(CLIENT_VERSION, "version-1".to_string());

            let error_id = Uuid::new_v4();
            let new_response = count_subgraph_errors(
                SubgraphResponse::fake_builder()
                    .context(context)
                    .subgraph_name("some-subgraph".to_string())
                    .status_code(StatusCode::BAD_REQUEST)
                    .errors(vec![
                        graphql::Error::builder()
                            .message("You did a bad request.")
                            .path(Path::from("obj/field"))
                            .extension_code("GRAPHQL_VALIDATION_FAILED")
                            .extension("service", "some-subgraph")
                            .apollo_id(error_id)
                            .build(),
                    ])
                    .build(),
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
                "graphql.error.path" = "/obj/field",
                "apollo.router.error.service" = "some-subgraph"
            );

            assert_counter!(
                "apollo.router.graphql_error",
                1,
                code = "GRAPHQL_VALIDATION_FAILED"
            );

            assert_eq!(
                unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
                HashSet::from([error_id])
            )
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_count_subgraph_errors_with_include_subgraphs_disabled() {
        async {
            let config = ErrorsConfiguration {
                preview_extended_error_metrics: ExtendedErrorMetricsMode::Enabled,
                subgraph: SubgraphErrorConfig {
                    subgraphs: HashMap::from([(
                        "some-subgraph".to_string(),
                        ErrorConfiguration {
                            send: false,
                            redact: true,
                            redaction_policy: ErrorRedactionPolicy::Strict,
                        },
                    )]),
                    ..Default::default()
                },
                ..Default::default()
            };

            let context = Context::default();
            let error_id = Uuid::new_v4();
            let new_response = count_subgraph_errors(
                SubgraphResponse::fake_builder()
                    .context(context)
                    .subgraph_name("some-subgraph".to_string())
                    .status_code(StatusCode::BAD_REQUEST)
                    .errors(vec![
                        graphql::Error::builder()
                            .message("You did a bad request.")
                            .path(Path::from("obj/field"))
                            .extension_code("GRAPHQL_VALIDATION_FAILED")
                            .extension("service", "some-subgraph")
                            .apollo_id(error_id)
                            .build(),
                    ])
                    .build(),
                &config,
            )
            .await;

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
                "graphql.error.path" = "/obj/field",
                "apollo.router.error.service" = "some-subgraph"
            );

            assert_counter!(
                // TODO(tim): is this a bug?  Should we not count these when the subgraph is excluded?
                "apollo.router.graphql_error",
                1,
                code = "GRAPHQL_VALIDATION_FAILED"
            );

            assert_eq!(
                unwrap_from_context::<HashSet<Uuid>>(&new_response.context, COUNTED_ERRORS),
                HashSet::from([error_id])
            )
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

            // Code is ignored for null, arrays, booleans and objects

            assert_counter!(
                "apollo.router.operations.error",
                4,
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

            assert_counter!("apollo.router.graphql_error", 4, code = "");

            assert_counter!(
                "apollo.router.operations.error",
                4,
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

            assert_counter!("apollo.router.graphql_error", 4);
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
