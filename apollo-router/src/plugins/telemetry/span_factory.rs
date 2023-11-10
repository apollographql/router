use schemars::JsonSchema;
use serde::Deserialize;
use tracing::error_span;
use tracing::info_span;

use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;
use crate::services::SubgraphRequest;
use crate::services::SupergraphRequest;
use crate::tracer::TraceId;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;

#[derive(Debug, Copy, Clone, Deserialize, JsonSchema, Default, Eq, PartialEq)]
/// Span mode to create new or deprecated spans
#[serde(rename_all = "snake_case")]
pub(crate) enum SpanMode {
    /// Keep the request span as root span and deprecated attributes. This option will eventually removed.
    #[default]
    Deprecated,
    /// Use new OpenTelemetry spec compliant span attributes or preserve existing. This will be the default in future.
    SpecCompliant,
}

impl SpanMode {
    pub(crate) fn create_request<B>(
        &self,
        request: &http::Request<B>,
        license_state: LicenseState,
    ) -> ::tracing::span::Span {
        match self {
            SpanMode::Deprecated => {
                if matches!(
                    license_state,
                    LicenseState::LicensedWarn | LicenseState::LicensedHalt
                ) {
                    error_span!(
                        REQUEST_SPAN_NAME,
                        "http.method" = %request.method(),
                        "http.request.method" = %request.method(),
                        "http.route" = %request.uri(),
                        "http.flavor" = ?request.version(),
                        "http.status" = 500, // This prevents setting later
                        "otel.name" = ::tracing::field::Empty,
                        "otel.kind" = "SERVER",
                        "graphql.operation.name" = ::tracing::field::Empty,
                        "graphql.operation.type" = ::tracing::field::Empty,
                        "apollo_router.license" = LICENSE_EXPIRED_SHORT_MESSAGE,
                        "apollo_private.request" = true,
                    )
                } else {
                    info_span!(
                        REQUEST_SPAN_NAME,
                        "http.method" = %request.method(),
                        "http.request.method" = %request.method(),
                        "http.route" = %request.uri(),
                        "http.flavor" = ?request.version(),
                        "otel.name" = ::tracing::field::Empty,
                        "otel.kind" = "SERVER",
                        "graphql.operation.name" = ::tracing::field::Empty,
                        "graphql.operation.type" = ::tracing::field::Empty,
                        "apollo_private.request" = true,
                    )
                }
            }
            SpanMode::SpecCompliant => {
                unreachable!("this code path should not be reachable, this is a bug!")
            }
        }
    }

    pub(crate) fn create_router<B>(&self, request: &http::Request<B>) -> ::tracing::span::Span {
        match self {
            SpanMode::Deprecated => {
                let trace_id = TraceId::maybe_new()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                let span = info_span!(ROUTER_SPAN_NAME,
                    "http.method" = %request.method(),
                    "http.request.method" = %request.method(),
                    "http.route" = %request.uri(),
                    "http.flavor" = ?request.version(),
                    "trace_id" = %trace_id,
                    "client.name" = ::tracing::field::Empty,
                    "client.version" = ::tracing::field::Empty,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                    "apollo_private.duration_ns" = ::tracing::field::Empty,
                    "apollo_private.http.request_headers" = ::tracing::field::Empty,
                    "apollo_private.http.response_headers" = ::tracing::field::Empty
                );
                span
            }
            SpanMode::SpecCompliant => {
                info_span!(ROUTER_SPAN_NAME,
                    // Needed for apollo_telemetry
                    "http.route" = %request.uri(),
                    "http.request.method" = %request.method(),
                    "otel.name" = ::tracing::field::Empty,
                    "otel.kind" = "SERVER",
                    "otel.status_code" = ::tracing::field::Empty,
                    "apollo_router.license" = ::tracing::field::Empty,
                    "apollo_private.duration_ns" = ::tracing::field::Empty,
                    "apollo_private.http.request_headers" = ::tracing::field::Empty,
                    "apollo_private.http.response_headers" = ::tracing::field::Empty,
                    "apollo_private.request" = true,
                )
            }
        }
    }

    pub(crate) fn create_supergraph(
        &self,
        config: &crate::plugins::telemetry::apollo::Config,
        request: &SupergraphRequest,
        field_level_instrumentation_ratio: f64,
    ) -> ::tracing::span::Span {
        match self {
            SpanMode::Deprecated => {
                let send_variable_values = config.send_variable_values.clone();
                let span = info_span!(
                    SUPERGRAPH_SPAN_NAME,
                    otel.kind = "INTERNAL",
                    graphql.operation.name = ::tracing::field::Empty,
                    graphql.document = request
                        .supergraph_request
                        .body()
                        .query
                        .as_deref()
                        .unwrap_or_default(),
                    apollo_private.field_level_instrumentation_ratio =
                        field_level_instrumentation_ratio,
                    apollo_private.operation_signature = ::tracing::field::Empty,
                    apollo_private.graphql.variables = Telemetry::filter_variables_values(
                        &request.supergraph_request.body().variables,
                        &send_variable_values,
                    ),
                );

                if let Some(operation_name) = request
                    .context
                    .get::<_, String>(OPERATION_NAME)
                    .unwrap_or_default()
                {
                    span.record("graphql.operation.name", operation_name);
                }
                span
            }
            SpanMode::SpecCompliant => {
                let send_variable_values = config.send_variable_values.clone();
                info_span!(
                    SUPERGRAPH_SPAN_NAME,
                    "otel.kind" = "INTERNAL",
                    apollo_private.field_level_instrumentation_ratio =
                        field_level_instrumentation_ratio,
                    apollo_private.operation_signature = ::tracing::field::Empty,
                    apollo_private.graphql.variables = Telemetry::filter_variables_values(
                        &request.supergraph_request.body().variables,
                        &send_variable_values,
                    )
                )
            }
        }
    }

    pub(crate) fn create_subgraph(
        &self,
        subgraph_name: &str,
        req: &SubgraphRequest,
    ) -> ::tracing::span::Span {
        match self {
            SpanMode::Deprecated => {
                let query = req
                    .subgraph_request
                    .body()
                    .query
                    .as_deref()
                    .unwrap_or_default();
                let operation_name = req
                    .subgraph_request
                    .body()
                    .operation_name
                    .as_deref()
                    .unwrap_or_default();

                info_span!(
                    SUBGRAPH_SPAN_NAME,
                    "apollo.subgraph.name" = subgraph_name,
                    graphql.document = query,
                    graphql.operation.name = operation_name,
                    "otel.kind" = "INTERNAL",
                    "apollo_private.ftv1" = ::tracing::field::Empty,
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
            SpanMode::SpecCompliant => {
                info_span!(
                    SUBGRAPH_SPAN_NAME,
                    "otel.kind" = "INTERNAL",
                    "apollo_private.ftv1" = ::tracing::field::Empty,
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
        }
    }
}
