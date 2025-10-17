use schemars::JsonSchema;
use serde::Deserialize;
use tracing::error_span;
use tracing::info_span;

use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::consts::CONNECT_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::consts::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
use crate::services::SubgraphRequest;
use crate::services::SupergraphRequest;
use crate::tracer::TraceId;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;
use crate::uplink::license_enforcement::LicenseState;

#[derive(Debug, Copy, Clone, Deserialize, JsonSchema, Default, Eq, PartialEq)]
/// Span mode to create new or deprecated spans
#[serde(rename_all = "snake_case")]
pub(crate) enum SpanMode {
    /// Keep the request span as root span and deprecated attributes. This option will eventually removed.
    Deprecated,
    /// Use new OpenTelemetry spec compliant span attributes or preserve existing. This will be the default in future.
    #[default]
    SpecCompliant,
}

impl SpanMode {
    pub(crate) fn create_request<B>(
        &self,
        request: &http::Request<B>,
        license_state: &LicenseState,
    ) -> ::tracing::span::Span {
        match self {
            SpanMode::Deprecated => {
                if matches!(
                    license_state,
                    LicenseState::LicensedWarn { limits: _ }
                        | LicenseState::LicensedHalt { limits: _ }
                ) {
                    error_span!(
                        REQUEST_SPAN_NAME,
                        "http.method" = %request.method(),
                        "http.request.method" = %request.method(),
                        "http.route" = %request.uri().path(),
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
                        "http.route" = %request.uri().path(),
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
                    "http.route" = %request.uri().path(),
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
                    // Needed for apollo_telemetry and datadog span mapping
                    "http.route" = %request.uri().path(),
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

    pub(crate) fn create_connector(&self, source_name: &str) -> ::tracing::span::Span {
        match self {
            SpanMode::Deprecated => {
                info_span!(
                    CONNECT_REQUEST_SPAN_NAME,
                    "apollo.source.name" = source_name,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
            SpanMode::SpecCompliant => {
                info_span!(
                    CONNECT_REQUEST_SPAN_NAME,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = ::tracing::field::Empty,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_mock::expect;
    use tracing_mock::subscriber;

    use crate::plugins::telemetry::SpanMode;
    use crate::plugins::telemetry::consts::REQUEST_SPAN_NAME;
    use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
    use crate::uplink::license_enforcement::LicenseState;

    #[test]
    fn test_specific_span() {
        // NB: this test checks the behavior of tracing_mock for a specific span.
        //  Most tests should probably follow the pattern of `test_http_route_on_array_of_router_spans`
        //  where they check a behavior across a variety of parameters.
        let request = http::Request::builder()
            .method("GET")
            .uri("http://example.com/path/to/location?with=query&another=UN1QU3_query")
            .header("apollographql-client-name", "client")
            .body("useful info")
            .unwrap();

        let expected_fields = expect::field("http.route")
            .with_value(&tracing::field::display("/path/to/location"))
            .and(expect::field("http.request.method").with_value(&tracing::field::display("GET")))
            .and(expect::field("otel.kind").with_value(&"SERVER"))
            .and(expect::field("apollo_private.request").with_value(&true));

        let expected_span = expect::span()
            .named(ROUTER_SPAN_NAME)
            .with_fields(expected_fields);

        let (subscriber, handle) = subscriber::mock()
            .new_span(expected_span)
            .enter(ROUTER_SPAN_NAME)
            .event(expect::event())
            .exit(ROUTER_SPAN_NAME)
            .run_with_handle();
        tracing::subscriber::with_default(subscriber, || {
            let span = SpanMode::SpecCompliant.create_router(&request);
            let _guard = span.enter();
            tracing::info!("an event happened!");
        });
        handle.assert_finished();
    }

    #[test]
    fn test_http_route_on_array_of_router_spans() {
        let expected_routes = [
            ("https://www.example.com/", "/"),
            ("https://www.example.com/path", "/path"),
            ("http://example.com/path/to/location", "/path/to/location"),
            ("http://www.example.com/path?with=query", "/path"),
            ("/foo/bar?baz", "/foo/bar"),
        ];

        let span_modes = [SpanMode::SpecCompliant, SpanMode::Deprecated];
        let license_states = [
            LicenseState::LicensedHalt { limits: None },
            LicenseState::Unlicensed,
        ];

        for (uri, expected_route) in expected_routes {
            let request = http::Request::builder().uri(uri).body("").unwrap();

            // test `request` spans
            for license_state in &license_states {
                let expected_span = expect::span().named(REQUEST_SPAN_NAME).with_fields(
                    expect::field("http.route")
                        .with_value(&tracing::field::display(expected_route)),
                );

                let span_mode = SpanMode::Deprecated;
                let (subscriber, handle) =
                    subscriber::mock().new_span(expected_span).run_with_handle();
                tracing::subscriber::with_default(subscriber, || {
                    let span = span_mode.create_request(&request, license_state);
                    let _guard = span.enter();
                });
                handle.assert_finished();
            }

            // test `router` spans
            for span_mode in span_modes {
                let expected_span = expect::span().named(ROUTER_SPAN_NAME).with_fields(
                    expect::field("http.route")
                        .with_value(&tracing::field::display(expected_route)),
                );

                let (subscriber, handle) =
                    subscriber::mock().new_span(expected_span).run_with_handle();
                tracing::subscriber::with_default(subscriber, || {
                    let span = span_mode.create_router(&request);
                    let _guard = span.enter();
                });
                handle.assert_finished();
            }
        }
    }
}
