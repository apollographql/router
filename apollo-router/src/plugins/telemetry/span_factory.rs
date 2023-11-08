use tracing::error_span;
use tracing::info_span;

use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::filter_headers;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;
use crate::services::SupergraphRequest;
use crate::tracer::TraceId;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;

#[derive(Debug, Copy, Clone)]
enum SpanFactory {
    Legacy,
    New,
}

impl SpanFactory {
    fn create_request<B>(
        &self,
        request: &http::Request<B>,
        license_state: LicenseState,
    ) -> ::tracing::span::Span {
        match self {
            SpanFactory::Legacy => {
                if matches!(
                    license_state,
                    LicenseState::LicensedWarn | LicenseState::LicensedHalt
                ) {
                    error_span!(
                        REQUEST_SPAN_NAME,
                        "http.method" = %request.method(),
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
            SpanFactory::New => {
                unreachable!("this code path should not be reachable, this is a bug!")
            }
        }
    }
    fn create_router<B>(
        &self,
        config: &crate::plugins::telemetry::apollo::Config,
        request: &http::Request<B>,
        license_state: LicenseState,
    ) -> ::tracing::span::Span {
        match self {
            SpanFactory::Legacy => {
                let trace_id = TraceId::maybe_new()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                let headers = request.headers();
                let client_name: &str = headers
                    .get(&config.client_name_header)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                let client_version = headers
                    .get(&config.client_version_header)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                let span = info_span!(ROUTER_SPAN_NAME,
                    "http.method" = %request.method(),
                    "http.route" = %request.uri(),
                    "http.flavor" = ?request.version(),
                    "trace_id" = %trace_id,
                    "client.name" = client_name,
                    "client.version" = client_version,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                    "apollo_private.duration_ns" = ::tracing::field::Empty,
                    "apollo_private.http.request_headers" = filter_headers(request.headers(), &config.send_headers).as_str(),
                    "apollo_private.http.response_headers" = ::tracing::field::Empty
                );
                span
            }
            SpanFactory::New => {
                if matches!(
                    license_state,
                    LicenseState::LicensedWarn | LicenseState::LicensedHalt
                ) {
                    info_span!(ROUTER_SPAN_NAME,
                        "http.route" = %request.uri(),
                        "otel.name" = ::tracing::field::Empty,
                        "http.response.status_code" = 500, // This prevents setting later
                        "otel.kind" = "SERVER",
                        "otel.status_code" = "Error",
                        "apollo_router.license" = LICENSE_EXPIRED_SHORT_MESSAGE,
                    )
                } else {
                    info_span!(ROUTER_SPAN_NAME,
                        "http.route" = %request.uri(),
                        "otel.name" = ::tracing::field::Empty,
                        "otel.kind" = "SERVER",
                        "otel.status_code" = ::tracing::field::Empty,
                    )
                }
            }
        }
    }
    fn create_supergraph(
        &self,
        config: &crate::plugins::telemetry::apollo::Config,
        request: &SupergraphRequest,
        field_level_instrumentation_ratio: f64,
    ) -> ::tracing::span::Span {
        match self {
            SpanFactory::Legacy => {
                let send_variable_values = config.send_variable_values.clone();
                let span = info_span!(
                    SUPERGRAPH_SPAN_NAME,
                    otel.kind = "INTERNAL",
                    graphql.operation.name = ::tracing::field::Empty,
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
            SpanFactory::New => {
                info_span!(
                    SUPERGRAPH_SPAN_NAME,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
        }
    }
    fn create_subgraph(&self, subgraph_name: &str) -> ::tracing::span::Span {
        match self {
            SpanFactory::Legacy => {
                info_span!(
                    SUBGRAPH_SPAN_NAME,
                    "apollo.subgraph.name" = subgraph_name,
                    "otel.kind" = "INTERNAL",
                    "apollo_private.ftv1" = ::tracing::field::Empty,
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
            SpanFactory::New => {
                info_span!(
                    SUBGRAPH_SPAN_NAME,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
        }
    }
}
