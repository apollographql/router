use crate::prelude::*;
use futures::future::BoxFuture;
use http::{
    header::{HeaderName, ACCEPT, CONTENT_TYPE},
    HeaderValue,
};
use hyper::client::HttpConnector;
use opentelemetry::{global, propagation::Injector};
use std::task::Poll;
use std::{str::FromStr, sync::Arc};
use tower::{BoxError, ServiceBuilder};
use tracing::{Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use typed_builder::TypedBuilder;

#[derive(TypedBuilder, Clone)]
pub struct TowerSubgraphService {
    client: hyper::Client<HttpConnector>,
    service: Arc<String>,
}

impl TowerSubgraphService {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            client: ServiceBuilder::new().service(hyper::Client::new()),
            service: Arc::new(service.into()),
        }
    }
}

impl tower::Service<graphql::SubgraphRequest> for TowerSubgraphService {
    type Response = graphql::SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map(|res| res.map_err(|e| Box::new(e) as BoxError))
    }

    fn call(&mut self, request: graphql::SubgraphRequest) -> Self::Future {
        let graphql::SubgraphRequest {
            http_request,
            context,
            ..
        } = request;

        let mut client = self.client.clone();
        let service_name = (*self.service).to_owned();

        //FIXME: header propagation
        Box::pin(async move {
            let (parts, body) = http_request.into_parts();

            let body = serde_json::to_string(&body).map_err(|error| {
                //FIXME: another error here?
                graphql::FetchError::SubrequestMalformedResponse {
                    service: service_name.clone(),
                    reason: error.to_string(),
                }
            })?;

            let mut request = http::request::Request::from_parts(parts, body.into());
            let app_json: HeaderValue = "application/json".parse().unwrap();
            request.headers_mut().insert(CONTENT_TYPE, app_json.clone());
            request.headers_mut().insert(ACCEPT, app_json);

            global::get_text_map_propagator(|injector| {
                injector.inject_context(
                    &Span::current().context(),
                    &mut RequestInjector {
                        request: &mut request,
                    },
                )
            });

            println!("tower[{}] will send {:?}", line!(), request);
            let response = client.call(request).await.map_err(|err| {
                tracing::error!(fetch_error = format!("{:?}", err).as_str());

                graphql::FetchError::SubrequestHttpError {
                    service: service_name.clone(),
                    reason: err.to_string(),
                }
            })?;
            println!("tower[{}] ", line!());

            let body = hyper::body::to_bytes(response.into_body())
                .instrument(tracing::debug_span!("aggregate_response_data"))
                .await
                .map_err(|err| {
                    tracing::error!(fetch_error = format!("{:?}", err).as_str());

                    graphql::FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;
            println!(
                "tower[{}]body:\n{}",
                line!(),
                std::str::from_utf8(&body).unwrap()
            );

            let graphql: graphql::Response = tracing::debug_span!("parse_subgraph_response")
                .in_scope(|| {
                    graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                        graphql::FetchError::SubrequestMalformedResponse {
                            service: service_name.clone(),
                            reason: error.to_string(),
                        }
                    })
                })?;
            println!("tower[{}] res={:?}", line!(), graphql);

            Ok(graphql::SubgraphResponse {
                response: http::Response::builder().body(graphql).expect("no argument can fail to parse or converted to the internal representation here; qed").into(),
                context,
            })
        })
    }
}

struct RequestInjector<'a, T> {
    request: &'a mut http::Request<T>,
}

impl<'a, T> Injector for RequestInjector<'a, T> {
    fn set(&mut self, key: &str, value: String) {
        let header_name = HeaderName::from_str(key).expect("Must be header name");
        let header_value = HeaderValue::from_str(&value).expect("Must be a header value");
        self.request.headers_mut().insert(header_name, header_value);
    }
}
/*

/// Injects the given Opentelemetry Context into a reqwest::Request headers to allow propagation downstream.
pub fn inject_opentracing_context_into_request(span: &Span, request: Request) -> Request {
    let context = span.context();
    let mut request = request;

    global::get_text_map_propagator(|injector| {
        injector.inject_context(&context, &mut RequestCarrier::new(&mut request))
    });

    request
}

// "traceparent" => https://www.w3.org/TR/trace-context/#trace-context-http-headers-format

/// Injector used via opentelemetry propagator to tell the extractor how to insert the "traceparent" header value
/// This will allow the propagator to inject opentelemetry context into a standard data structure. Will basically
/// insert a "traceparent" string value "{version}-{trace_id}-{span_id}-{trace-flags}" of the spans context into the headers.
/// Listeners can then re-hydrate the context to add additional spans to the same trace.
struct RequestCarrier<'a> {
    request: &'a mut Request,
}

impl<'a> RequestCarrier<'a> {
    pub fn new(request: &'a mut Request) -> Self {
        RequestCarrier { request }
    }
}

impl<'a> Injector for RequestCarrier<'a> {
    fn set(&mut self, key: &str, value: String) {
        let header_name = HeaderName::from_str(key).expect("Must be header name");
        let header_value = HeaderValue::from_str(&value).expect("Must be a header value");
        self.request.headers_mut().insert(header_name, header_value);
    }
}

*/
