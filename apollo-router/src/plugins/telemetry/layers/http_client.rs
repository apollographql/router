use std::sync::Arc;

use ::tracing::Span;
use opentelemetry::KeyValue;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Context;
use crate::layers::ServiceBuilderExt;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::router_overhead;
use crate::plugins::telemetry::consts::HTTP_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;

/// Layer type for [Telemetry::instrument_http_client_layer].
#[derive(Clone)]
pub(crate) struct InstrumentHttpClientLayer {
    _private: (),
}

impl InstrumentHttpClientLayer {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for InstrumentHttpClientLayer
where
    S: tower::Service<
            crate::services::http::HttpRequest,
            Response = crate::services::http::HttpResponse,
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = crate::services::http::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        ServiceBuilder::new()
            .instrument(move |request: &crate::services::http::HttpRequest| {
                let schema_uri = request.http_request.uri();
                let host = schema_uri.host().unwrap_or_default();
                let port = schema_uri.port_u16().unwrap_or_else(|| {
                    let scheme = schema_uri.scheme_str();
                    if scheme == Some("https") {
                        443
                    } else if scheme == Some("http") {
                        80
                    } else {
                        0
                    }
                });

                let path = schema_uri.path();
                ::tracing::info_span!(HTTP_REQUEST_SPAN_NAME,
                    "otel.kind" = "CLIENT",
                    "net.peer.name" = %host,
                    "net.peer.port" = %port,
                    "http.route" = %path,
                    "http.url" = %schema_uri,
                    "net.transport" = "ip_tcp",
                )
            })
            .service(inner)
            .boxed_clone()
    }
}

/// Layer type for [Telemetry::custom_instrument_http_client_layer].
#[derive(Clone)]
pub(crate) struct CustomInstrumentHttpClientLayer {
    config: Arc<config::Conf>,
}

impl CustomInstrumentHttpClientLayer {
    fn new(config: Arc<config::Conf>) -> Self {
        Self { config }
    }
}

impl<S> tower::Layer<S> for CustomInstrumentHttpClientLayer
where
    S: tower::Service<
            crate::services::http::HttpRequest,
            Response = crate::services::http::HttpResponse,
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = crate::services::http::BoxCloneService;

    fn layer(&self, inner: S) -> Self::Service {
        let req_fn_config = self.config.clone();
        let res_fn_config = self.config.clone();

        ServiceBuilder::new()
            .map_future_with_request_data(
                move |request: &crate::services::http::HttpRequest| {
                    // the HTTP request span's attributes, made only where the span is built
                    let custom_span_attributes = if Span::current().is_disabled() {
                        Vec::new()
                    } else {
                        req_fn_config
                            .instrumentation
                            .spans
                            .http_client
                            .attributes
                            .on_request(request)
                    };

                    (request.context.clone(), custom_span_attributes)
                },
                move |(context, custom_span_attributes): (Context, Vec<KeyValue>), f| {
                    let conf = res_fn_config.clone();
                    async move {
                        let span = Span::current();
                        span.set_span_dyn_attributes(custom_span_attributes);

                        let result: Result<crate::services::http::HttpResponse, BoxError> = f.await;
                        match &result {
                            Ok(response) => {
                                span.set_span_dyn_attributes_from(|| {
                                    conf.instrumentation
                                        .spans
                                        .http_client
                                        .attributes
                                        .on_response(response)
                                });
                            }
                            Err(err) => {
                                span.set_span_dyn_attributes_from(|| {
                                    conf.instrumentation
                                        .spans
                                        .http_client
                                        .attributes
                                        .on_error(err, &context)
                                });
                            }
                        }
                        result
                    }
                },
            )
            .service(inner)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that holds a router overhead subrequest guard while its inner service is in
    /// progress, so that time spent waiting for subgraph or connector responses is not included in
    /// the router overhead metrics.
    ///
    /// Placed where the router overhead is read: by the overhead histogram, or by a router
    /// selector of the overhead in an instrument, an event or a span attribute.
    pub(crate) fn overhead_subgraph_request_timing_layer(
        &self,
    ) -> Option<router_overhead::OverheadLayer> {
        self.readers()
            .router_overhead()
            .then(router_overhead::OverheadLayer::new)
    }

    /// Returns a layer that instruments an HTTP client service with an `http_request` span, where
    /// spans are built.
    pub(crate) fn instrument_http_client_layer(&self) -> Option<InstrumentHttpClientLayer> {
        self.readers().spans.then(InstrumentHttpClientLayer::new)
    }

    /// Returns a layer that applies user-configured custom attributes to the active span, where
    /// spans are built.
    pub(crate) fn custom_instrument_http_client_layer(
        &self,
    ) -> Option<CustomInstrumentHttpClientLayer> {
        self.readers()
            .spans
            .then(|| CustomInstrumentHttpClientLayer::new(self.config.clone()))
    }
}
