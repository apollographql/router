//! HTTP client utilities for subgraph communication

use bytes::Bytes;
use futures::TryFutureExt;
use http::HeaderValue;
use http::Request;
use http::header;
use http::response::Parts;
use mediatype::MediaType;
use mediatype::names::APPLICATION;
use mediatype::names::JSON;
use mime::APPLICATION_JSON;
use tower::Service;
use tracing::Instrument;

use crate::Context;
use crate::error::FetchError;
use crate::graphql;
use crate::plugins::content_negotiation::APPLICATION_GRAPHQL_JSON;
use crate::services::http::HttpRequest;
use crate::services::router::body::RouterBody;

pub(crate) static APPLICATION_JSON_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("application/json");
pub(crate) static ACCEPT_GRAPHQL_JSON: HeaderValue =
    HeaderValue::from_static("application/json, application/graphql-response+json");

const GRAPHQL_RESPONSE: mediatype::Name = mediatype::Name::new_unchecked("graphql-response");

#[derive(Clone, Debug)]
pub(crate) enum ContentType {
    ApplicationJson,
    ApplicationGraphqlResponseJson,
}

/// Utility function to extract uri details.
pub(crate) fn get_uri_details(uri: &hyper::Uri) -> (&str, u16, &str) {
    let port = uri.port_u16().unwrap_or_else(|| {
        let scheme = uri.scheme_str();
        if scheme == Some("https") {
            443
        } else if scheme == Some("http") {
            80
        } else {
            0
        }
    });

    (uri.host().unwrap_or_default(), port, uri.path())
}

/// Utility function to create a graphql response from HTTP response components
pub(crate) fn http_response_to_graphql_response(
    service_name: &str,
    content_type: Result<ContentType, FetchError>,
    body: Option<Result<Bytes, FetchError>>,
    parts: &Parts,
) -> graphql::Response {
    let mut graphql_response = match (content_type, body, parts.status.is_success()) {
        (Ok(ContentType::ApplicationGraphqlResponseJson), Some(Ok(body)), _)
        | (Ok(ContentType::ApplicationJson), Some(Ok(body)), true) => {
            // Application graphql json expects valid graphql response
            // Application json expects valid graphql response if 2xx
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                // Application graphql json expects valid graphql response
                graphql::Response::from_bytes(body).unwrap_or_else(|error| {
                    let error = FetchError::SubrequestMalformedResponse {
                        service: service_name.to_owned(),
                        reason: error.reason,
                    };
                    graphql::Response::builder()
                        .error(error.to_graphql_error(None))
                        .build()
                })
            })
        }
        (Ok(ContentType::ApplicationJson), Some(Ok(body)), false) => {
            // Application json does not expect a valid graphql response if not 2xx.
            // If parse fails then attach the entire payload as an error
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                // Application graphql json expects valid graphql response
                let mut original_response = String::from_utf8_lossy(&body).to_string();
                if original_response.is_empty() {
                    original_response = "<empty response body>".into()
                }
                graphql::Response::from_bytes(body).unwrap_or_else(|_error| {
                    graphql::Response::builder()
                        .error(
                            FetchError::SubrequestMalformedResponse {
                                service: service_name.to_string(),
                                reason: original_response,
                            }
                            .to_graphql_error(None),
                        )
                        .build()
                })
            })
        }
        (content_type, body, _) => {
            // Something went wrong, compose a response with errors if they are present
            let mut graphql_response = graphql::Response::builder().build();
            if let Err(err) = content_type {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            if let Some(Err(err)) = body {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            graphql_response
        }
    };

    // Add an error for response codes that are not 2xx
    if !parts.status.is_success() {
        let status = parts.status;
        graphql_response.errors.insert(
            0,
            FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                status_code: Some(status.as_u16()),
                reason: format!(
                    "{}: {}",
                    status.as_str(),
                    status.canonical_reason().unwrap_or("Unknown")
                ),
            }
            .to_graphql_error(None),
        )
    }

    graphql_response
}

pub(crate) fn get_graphql_content_type(
    service_name: &str,
    parts: &Parts,
) -> Result<ContentType, FetchError> {
    if let Some(raw_content_type) = parts.headers.get(header::CONTENT_TYPE) {
        let content_type = raw_content_type
            .to_str()
            .ok()
            .and_then(|str| MediaType::parse(str).ok());

        match content_type {
            Some(mime) if mime.ty == APPLICATION && mime.subty == JSON => {
                Ok(ContentType::ApplicationJson)
            }
            Some(mime)
                if mime.ty == APPLICATION
                    && mime.subty == GRAPHQL_RESPONSE
                    && mime.suffix == Some(JSON) =>
            {
                Ok(ContentType::ApplicationGraphqlResponseJson)
            }
            Some(mime) => Err(format!(
                "subgraph response contains unsupported content-type: {}",
                mime,
            )),
            None => Err(format!(
                "subgraph response contains invalid 'content-type' header value {:?}",
                raw_content_type,
            )),
        }
    } else {
        Err("subgraph response does not contain 'content-type' header".to_owned())
    }
    .map_err(|reason| FetchError::SubrequestHttpError {
        status_code: Some(parts.status.as_u16()),
        service: service_name.to_string(),
        reason: format!(
            "{}; expected content-type: {} or content-type: {}",
            reason,
            APPLICATION_JSON.essence_str(),
            APPLICATION_GRAPHQL_JSON
        ),
    })
}

pub(crate) async fn do_fetch(
    mut client: crate::services::http::BoxService,
    context: &Context,
    service_name: &str,
    request: Request<RouterBody>,
) -> Result<
    (
        Parts,
        Result<ContentType, FetchError>,
        Option<Result<Bytes, FetchError>>,
    ),
    FetchError,
> {
    let response = client
        .call(HttpRequest {
            http_request: request,
            context: context.clone(),
        })
        .map_err(|err| {
            tracing::error!(fetch_error = ?err);
            FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name.to_string(),
                reason: err.to_string(),
            }
        })
        .await?;

    let (parts, body) = response.http_response.into_parts();

    let content_type = get_graphql_content_type(service_name, &parts);

    let body = if content_type.is_ok() {
        let body = crate::services::router::body::into_bytes(body)
            .instrument(tracing::debug_span!("aggregate_response_data"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = ?err);
                FetchError::SubrequestHttpError {
                    status_code: Some(parts.status.as_u16()),
                    service: service_name.to_string(),
                    reason: err.to_string(),
                }
            });
        Some(body)
    } else {
        None
    };
    Ok((parts, content_type, body))
}
