use tracing::info_span;

use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::consts::CONNECT_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::consts::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
use crate::services::SubgraphRequest;
use crate::services::SupergraphRequest;

pub(crate) fn create_router<B>(_request: &http::Request<B>) -> ::tracing::span::Span {
    info_span!(
        ROUTER_SPAN_NAME,
        // Note that http.route and http.request.method are always added by default,
        // but in the on_request selector logic in HttpServerAttributes and HttpCommonAttributes
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

/// Create a router span for a request that was rejected by hyper before reaching the axum
/// service layer (e.g. 431 / 414). No `http::Request` is available in that case, so this
/// variant omits request-derived fields and records only what we know at rejection time.
pub(crate) fn create_router_rejection() -> ::tracing::span::Span {
    info_span!(
        ROUTER_SPAN_NAME,
        "otel.name" = ::tracing::field::Empty,
        "otel.kind" = "SERVER",
        "otel.status_code" = ::tracing::field::Empty,
        "http.response.status_code" = ::tracing::field::Empty,
        "apollo_router.license" = ::tracing::field::Empty,
        "apollo_private.duration_ns" = ::tracing::field::Empty,
        "apollo_private.http.request_headers" = ::tracing::field::Empty,
        "apollo_private.http.response_headers" = ::tracing::field::Empty,
        "apollo_private.request" = true,
    )
}

pub(crate) fn create_supergraph(
    config: &crate::plugins::telemetry::apollo::Config,
    request: &SupergraphRequest,
    field_level_instrumentation_ratio: f64,
) -> ::tracing::span::Span {
    let send_variable_values = config.send_variable_values.clone();
    info_span!(
        SUPERGRAPH_SPAN_NAME,
        "otel.kind" = "INTERNAL",
        apollo_private.field_level_instrumentation_ratio = field_level_instrumentation_ratio,
        apollo_private.operation_signature = ::tracing::field::Empty,
        apollo_private.graphql.variables = Telemetry::filter_variables_values(
            &request.supergraph_request.body().variables,
            &send_variable_values,
        ),
    )
}

pub(crate) fn create_subgraph(
    _subgraph_name: &str,
    _req: &SubgraphRequest,
) -> ::tracing::span::Span {
    info_span!(
        SUBGRAPH_SPAN_NAME,
        "otel.kind" = "INTERNAL",
        "apollo_private.ftv1" = ::tracing::field::Empty,
        "otel.status_code" = ::tracing::field::Empty,
    )
}

pub(crate) fn create_connector(_source_name: &str) -> ::tracing::span::Span {
    info_span!(
        CONNECT_REQUEST_SPAN_NAME,
        "otel.kind" = "INTERNAL",
        "otel.status_code" = ::tracing::field::Empty,
        "apollo.connector.response.aborted" = ::tracing::field::Empty,
    )
}

#[cfg(test)]
mod tests {
    use tracing_mock::expect;
    use tracing_mock::subscriber;

    use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;

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

        // http.route and http.request.method are added by the on_request selector logic,
        // not at span creation time.
        let expected_fields = expect::field("otel.kind")
            .with_value(&"SERVER")
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
            let span = super::create_router(&request);
            let _guard = span.enter();
            tracing::info!("an event happened!");
        });
        handle.assert_finished();
    }
}
