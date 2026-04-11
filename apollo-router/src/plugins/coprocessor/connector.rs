//! Connector coprocessor stage implementation

use std::ops::ControlFlow;
use std::time::Instant;

use apollo_federation::connectors::runtime::errors::Error as ConnectorError;
use apollo_federation::connectors::runtime::errors::RuntimeError;
use apollo_federation::connectors::runtime::http_json_transport::HttpRequest as ConnectorsHttpRequest;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_federation::connectors::runtime::responses::MappedResponse;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;

use super::COPROCESSOR_ERROR_EXTENSION;
use super::ContextConf;
use super::EXTERNAL_SPAN_NAME;
use super::NewContextConf;
use super::internalize_header_map;
use super::record_coprocessor_duration;
use super::record_coprocessor_operation;
use super::update_context_from_coprocessor;
use super::validate_coprocessor_output;
use crate::Context;
use crate::context::context_key_from_deprecated;
use crate::json_ext::Value;
use crate::layers::ServiceBuilderExt;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::layers::map_future_with_request_data::MapFutureWithRequestDataLayer;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::connector::selectors::ConnectorSelector;
use crate::services::connector::request_service;
use crate::services::external::Control;
use crate::services::external::Externalizable;
use crate::services::external::PipelineStep;
use crate::services::external::externalize_header_map;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;

/// What information is passed to a connector request stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ConnectorRequestConf {
    /// Condition to trigger this stage
    pub(super) condition: Condition<ConnectorSelector>,
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: ContextConf,
    /// Send the body
    pub(super) body: bool,
    /// Send the connector URI
    pub(super) uri: bool,
    /// Send the method
    pub(super) method: bool,
    /// Send the service name
    pub(super) service_name: bool,
    /// The coprocessor URL for this stage (overrides the global URL if specified)
    pub(super) url: Option<String>,
}

/// What information is passed to a connector response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ConnectorResponseConf {
    /// Condition to trigger this stage
    pub(super) condition: Condition<ConnectorSelector>,
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: ContextConf,
    /// Send the body
    pub(super) body: bool,
    /// Send the service name
    pub(super) service_name: bool,
    /// Send the http status
    pub(super) status_code: bool,
    /// The coprocessor URL for this stage (overrides the global URL if specified)
    pub(super) url: Option<String>,
}

/// Configures the connector coprocessor stages
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default)]
pub(super) struct ConnectorStages {
    #[serde(default)]
    pub(super) all: ConnectorStage,
}

/// The connector stage request/response configuration
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ConnectorStage {
    /// The request configuration
    #[serde(default)]
    pub(super) request: ConnectorRequestConf,
    /// The response configuration
    #[serde(default)]
    pub(super) response: ConnectorResponseConf,
}

impl ConnectorStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: request_service::BoxService,
        default_url: String,
        service_name: String,
    ) -> request_service::BoxService
    where
        C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<HttpRequest>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let coprocessor_url = request_config.url.clone().unwrap_or(default_url.clone());
            let http_client = http_client.clone();
            let service_name = service_name.clone();

            AsyncCheckpointLayer::new(move |request: request_service::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let service_name = service_name.clone();

                async move {
                    let mut succeeded = true;
                    let mut executed = false;
                    let result = process_connector_request_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        request,
                        request_config,
                        &mut executed,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!("coprocessor: connector request stage error: {error}");
                        error
                    });
                    if executed {
                        record_coprocessor_operation(PipelineStep::ConnectorRequest, succeeded);
                    }
                    result
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();
            let coprocessor_url = response_config.url.clone().unwrap_or(default_url);
            let service_name = service_name.clone();

            MapFutureWithRequestDataLayer::new(
                |req: &request_service::Request| req.context.clone(),
                move |context: Context, fut| {
                    let http_client = http_client.clone();
                    let coprocessor_url = coprocessor_url.clone();
                    let response_config = response_config.clone();
                    let service_name = service_name.clone();

                    async move {
                        let response: request_service::Response = fut.await?;

                        let mut succeeded = true;
                        let mut executed = false;
                        let result = process_connector_response_stage(
                            http_client,
                            coprocessor_url,
                            service_name,
                            response,
                            response_config,
                            context,
                            &mut executed,
                        )
                        .await
                        .map_err(|error| {
                            succeeded = false;
                            tracing::error!("coprocessor: connector response stage error: {error}");
                            error
                        });
                        if executed {
                            record_coprocessor_operation(
                                PipelineStep::ConnectorResponse,
                                succeeded,
                            );
                        }
                        result
                    }
                },
            )
        });

        fn external_service_span() -> impl Fn(&request_service::Request) -> tracing::Span + Clone {
            move |_request: &request_service::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(request_service::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            .option_layer(response_layer)
            .buffered(
                "connector",
                vec![
                    opentelemetry::KeyValue::new("source.name", service_name.clone()),
                    opentelemetry::KeyValue::new("plugin.name", "coprocessor"),
                ],
            )
            .service(service)
            .boxed()
    }
}

/// This function receives a mutable `executed` flag so the caller can know
/// whether this stage actually ran before an early return or error. This is
/// required because metric recording happens outside this function.
///
/// Using `&mut` here is not the most idiomatic Rust pattern, but it was the
/// least intrusive way to expose this information without refactoring all
/// router stage processing functions.
async fn process_connector_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut request: request_service::Request,
    mut request_config: ConnectorRequestConf,
    executed: &mut bool,
) -> Result<ControlFlow<request_service::Response, request_service::Request>, BoxError>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    if request_config.condition.evaluate_request(&request) != Some(true) {
        return Ok(ControlFlow::Continue(request));
    }

    // Extract the transport request parts
    let TransportRequest::Http(http_request) = request.transport_request;
    let debug = http_request.debug;
    let (parts, body) = http_request.inner.into_parts();

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers));

    let body_to_send = request_config.body.then(|| {
        serde_json::from_str::<Value>(&body).unwrap_or_else(|_| Value::String(body.clone().into()))
    });

    let context_to_send = request_config.context.get_context(&request.context);
    let uri = request_config.uri.then(|| parts.uri.to_string());
    let service_name_to_send = request_config.service_name.then_some(service_name);

    let payload = Externalizable::connector_builder()
        .stage(PipelineStep::ConnectorRequest)
        .control(Control::default())
        .id(request.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .method(parts.method.to_string())
        .and_service_name(service_name_to_send)
        .and_uri(uri)
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();

    // We use a new context here to avoid any risk of carrying extensions to coprocessor calls that
    // we don't intend for coprocessor calls; if in the future we change it, make sure to
    // understand what could be sent to coprocessors and how that might affect their behavior
    let co_processor_result = payload
        .call(http_client, &coprocessor_url, Context::new())
        .await;
    *executed = true;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::ConnectorRequest, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;
    validate_coprocessor_output(&co_processor_output, PipelineStep::ConnectorRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    if matches!(control, Control::Break(_)) {
        let body = co_processor_output.body.unwrap_or(Value::Null);

        const DEFAULT_BREAK_MESSAGE: &str = "Internal error";

        let (message, code, extra_extensions) = match body {
            Value::String(s) if !s.as_str().is_empty() => (
                s.as_str().to_owned(),
                COPROCESSOR_ERROR_EXTENSION.to_string(),
                serde_json_bytes::Map::default(),
            ),
            Value::Object(ref obj) => parse_connector_break_error(obj),
            Value::Null | Value::String(_) => (
                DEFAULT_BREAK_MESSAGE.to_string(),
                COPROCESSOR_ERROR_EXTENSION.to_string(),
                serde_json_bytes::Map::default(),
            ),
            other => (
                other.to_string(),
                COPROCESSOR_ERROR_EXTENSION.to_string(),
                serde_json_bytes::Map::default(),
            ),
        };

        let mut runtime_error = RuntimeError::new(&message, &request.key).with_code(code);
        for (k, v) in extra_extensions {
            runtime_error = runtime_error.extension(k, v);
        }

        let res = request_service::Response {
            context: request.context.clone(),
            transport_result: Err(ConnectorError::TransportFailure(message)),
            mapped_response: MappedResponse::Error {
                error: runtime_error,
                key: request.key,
                problems: Vec::new(),
            },
        };

        if let Some(context) = co_processor_output.context {
            for (mut key, value) in context.try_into_iter()? {
                if let ContextConf::NewContextConf(NewContextConf::Deprecated) =
                    &request_config.context
                {
                    key = context_key_from_deprecated(key);
                }
                request
                    .context
                    .upsert_json_value(key, move |_current| value);
            }
        }

        return Ok(ControlFlow::Break(res));
    }

    // Continue flow - apply modifications: Body, headers, uri, and context.
    let new_body = match co_processor_output.body {
        Some(Value::String(s)) => s.as_str().to_owned(),
        Some(other) => other.to_string(),
        None => body,
    };

    let mut new_parts = parts;

    if let Some(headers) = co_processor_output.headers {
        new_parts.headers = internalize_header_map(headers)?;
    }

    if let Some(uri) = co_processor_output.uri {
        new_parts.uri = uri.parse()?;
    }

    if let Some(context) = co_processor_output.context {
        for (mut key, value) in context.try_into_iter()? {
            if let ContextConf::NewContextConf(NewContextConf::Deprecated) = &request_config.context
            {
                key = context_key_from_deprecated(key);
            }
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    // Reconstruct the transport request
    request.transport_request = TransportRequest::Http(ConnectorsHttpRequest {
        inner: http::Request::from_parts(new_parts, new_body),
        debug,
    });

    Ok(ControlFlow::Continue(request))
}

/// This function receives a mutable `executed` flag so the caller can know
/// whether this stage actually ran before an early return or error. This is
/// required because metric recording happens outside this function.
///
/// Using `&mut` here is not the most idiomatic Rust pattern, but it was the
/// least intrusive way to expose this information without refactoring all
/// router stage processing functions.
async fn process_connector_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut response: request_service::Response,
    response_config: ConnectorResponseConf,
    context: Context,
    executed: &mut bool,
) -> Result<request_service::Response, BoxError>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    if !response_config.condition.evaluate_response(&response) {
        return Ok(response);
    }

    // Extract data from the transport result
    let (headers_to_send, status_to_send) = match &response.transport_result {
        Ok(TransportResponse::Http(http_response)) => {
            let headers = response_config
                .headers
                .then(|| externalize_header_map(&http_response.inner.headers));
            let status = response_config
                .status_code
                .then(|| http_response.inner.status.as_u16());
            (headers, status)
        }
        Err(_) => (None, None),
    };

    // Extract body from mapped response
    let body_to_send: Option<serde_json_bytes::Value> = if response_config.body {
        match &response.mapped_response {
            MappedResponse::Data { data, .. } => Some(data.clone()),
            MappedResponse::Error { error, .. } => Some(serde_json_bytes::json!({
                "errors": [{"message": error.message.clone()}]
            })),
        }
    } else {
        None
    };

    let context_to_send = response_config.context.get_context(&context);
    let service_name_to_send = response_config.service_name.then_some(service_name);

    let payload = Externalizable::connector_builder()
        .stage(PipelineStep::ConnectorResponse)
        .id(context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_service_name(service_name_to_send)
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();

    // We use a new context here to avoid any risk of carrying extensions to coprocessor calls that
    // we don't intend for coprocessor calls; if in the future we change it, make sure to
    // understand what could be sent to coprocessors and how that might affect their behavior
    let co_processor_result = payload
        .call(http_client, &coprocessor_url, Context::new())
        .await;
    *executed = true;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::ConnectorResponse, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::ConnectorResponse)?;

    // Apply modifications to the response
    if let Some(control) = co_processor_output.control {
        let new_status = control.get_http_status()?;
        // Update the transport result status if it was successful
        if let Ok(TransportResponse::Http(ref mut http_response)) = response.transport_result {
            http_response.inner.status = new_status;
        }
    }

    if let Some(headers) = co_processor_output.headers
        && let Ok(TransportResponse::Http(ref mut http_response)) = response.transport_result
    {
        http_response.inner.headers = internalize_header_map(headers)?;
    }

    if let Some(returned_context) = co_processor_output.context {
        update_context_from_coprocessor(&context, returned_context, &response_config.context)?;
    }

    if let Some(body) = co_processor_output.body {
        match response.mapped_response {
            MappedResponse::Data { ref mut data, .. } => {
                *data = body;
            }
            MappedResponse::Error {
                mut error,
                key,
                problems,
            } => {
                if let Some(errors) = body.get("errors").and_then(|e| e.as_array())
                    && let Some(first_error) = errors.first().and_then(|e| e.as_object())
                {
                    if let Some(message) = first_error.get("message").and_then(|m| m.as_str()) {
                        error.message = message.to_string();
                    }
                    if let Some(code) = first_error
                        .get("extensions")
                        .and_then(|e| e.as_object())
                        .and_then(|ext| ext.get("code"))
                        .and_then(|c| c.as_str())
                    {
                        error = error.with_code(code);
                    }
                }
                response.mapped_response = MappedResponse::Error {
                    error,
                    key,
                    problems,
                };
            }
        }
    }

    Ok(response)
}

/// Parse structured error from a coprocessor break response body.
/// Expects a JSON object with an `"errors"` array containing GraphQL-style errors.
/// Returns `(message, code, extra_extensions)`.
fn parse_connector_break_error(
    obj: &serde_json_bytes::Map<ByteString, Value>,
) -> (String, String, serde_json_bytes::Map<ByteString, Value>) {
    let default_code = COPROCESSOR_ERROR_EXTENSION.to_string();
    let default_msg = "Internal error".to_string();

    let errors = match obj.get("errors") {
        Some(Value::Array(arr)) if !arr.is_empty() => arr,
        _ => return (default_msg, default_code, Default::default()),
    };

    let first_error = match errors.first() {
        Some(Value::Object(e)) => e,
        _ => return (default_msg, default_code, Default::default()),
    };

    let message = first_error
        .get("message")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(default_msg);

    let mut extra_extensions = serde_json_bytes::Map::default();
    let code = if let Some(Value::Object(ext)) = first_error.get("extensions") {
        let code = ext
            .get("code")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or(default_code);
        for (k, v) in ext.iter() {
            if k.as_str() != "code" {
                extra_extensions.insert(k.clone(), v.clone());
            }
        }
        code
    } else {
        default_code
    };

    (message, code, extra_extensions)
}
