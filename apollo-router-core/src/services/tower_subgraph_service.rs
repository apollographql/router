//! Tower fetcher for subgraphs.

use crate::prelude::*;
use futures::future::BoxFuture;
use global::get_text_map_propagator;
use http::{
    header::{ACCEPT, CONTENT_TYPE},
    HeaderValue,
};
use hyper::client::HttpConnector;
use hyper_rustls::HttpsConnector;
use opentelemetry::global;
use std::sync::Arc;
use std::task::Poll;
use tower::{BoxError, ServiceBuilder};
use tracing::{Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub struct TowerSubgraphService {
    client: hyper::Client<HttpsConnector<HttpConnector>>,
    service: Arc<String>,
}

impl TowerSubgraphService {
    pub fn new(service: impl Into<String>) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        Self {
            client: ServiceBuilder::new().service(hyper::Client::builder().build(connector)),
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
            subgraph_request,
            context,
            ..
        } = request;

        let mut client = self.client.clone();
        let service_name = (*self.service).to_owned();

        Box::pin(async move {
            let (parts, body) = subgraph_request.into_parts();

            let body = serde_json::to_string(&body).expect("JSON serialization should not fail");

            let mut request = http::request::Request::from_parts(parts, body.into());
            let app_json: HeaderValue = "application/json".parse().unwrap();
            request.headers_mut().insert(CONTENT_TYPE, app_json.clone());
            request.headers_mut().insert(ACCEPT, app_json);

            get_text_map_propagator(|propagator| {
                propagator.inject_context(
                    &Span::current().context(),
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            });

            let response = client.call(request).await.map_err(|err| {
                tracing::error!(fetch_error = format!("{:?}", err).as_str());

                graphql::FetchError::SubrequestHttpError {
                    service: service_name.clone(),
                    reason: err.to_string(),
                }
            })?;

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

            let graphql: graphql::Response = tracing::debug_span!("parse_subgraph_response")
                .in_scope(|| {
                    graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                        graphql::FetchError::SubrequestMalformedResponse {
                            service: service_name.clone(),
                            reason: error.to_string(),
                        }
                    })
                })?;

            Ok(graphql::SubgraphResponse::new_from_response(
                http::Response::builder().body(graphql).expect("no argument can fail to parse or converted to the internal representation here; qed").into(),
                context,
            ))
        })
    }
}
