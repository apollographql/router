//! HTTP client utilities used by the subgraph [`super::service`].
//!
//! This covers building the TLS client config used to talk to subgraphs, and translating raw
//! HTTP responses coming back from a subgraph into `graphql::Response`s.

use bytes::Bytes;
use futures::TryFutureExt;
use http::Request;
use http::response::Parts;
use http_body_util::LengthLimitError;
use hyper_rustls::ConfigBuilderExt;
use rustls::RootCertStore;
use tower::BoxError;
use tower::Service;
use tracing::Instrument;

use crate::Context;
use crate::configuration::TlsClientAuth;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::ObjectExt;
use crate::plugins::limits::SubgraphResponseSizeLimit;
use crate::services::http::HttpRequest;
use crate::services::layers::content_negotiation::ContentType;
use crate::services::layers::content_negotiation::get_graphql_content_type;
use crate::services::router;
use crate::services::router::body::RouterBody;

#[allow(clippy::declare_interior_mutable_const)]
pub(crate) static APPLICATION_JSON_HEADER_VALUE: http::HeaderValue =
    http::HeaderValue::from_static("application/json");

pub(crate) fn generate_tls_client_config(
    tls_cert_store: Option<RootCertStore>,
    client_cert_config: Option<&TlsClientAuth>,
) -> Result<rustls::ClientConfig, BoxError> {
    let tls_builder = rustls::ClientConfig::builder();
    Ok(match (tls_cert_store, client_cert_config) {
        (None, None) => tls_builder.with_native_roots()?.with_no_client_auth(),
        (Some(store), None) => tls_builder
            .with_root_certificates(store)
            .with_no_client_auth(),
        (None, Some(client_auth_config)) => {
            tls_builder.with_native_roots()?.with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone_key(),
            )?
        }
        (Some(store), Some(client_auth_config)) => tls_builder
            .with_root_certificates(store)
            .with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone_key(),
            )?,
    })
}

// Utility function to extract uri details.
pub(super) fn get_uri_details(uri: &hyper::Uri) -> (&str, u16, &str) {
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

pub(super) fn http_response_to_graphql_response(
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

    // Any errors directly parsed from the response likely won't yet have the service name set,
    // but we need it for telemetry error counting
    for err in &mut graphql_response.errors {
        err.extensions.object_insert_if_absent("service", service_name);
    }

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

pub(super) async fn do_fetch(
    mut client: crate::services::http::BoxCloneService,
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

    let response_size_limit = context
        .extensions()
        .with_lock(|e| e.get::<SubgraphResponseSizeLimit>().copied());

    let body = if content_type.is_ok() {
        let body_result = match response_size_limit {
            Some(SubgraphResponseSizeLimit(limit)) => {
                router::body::into_bytes_limited(body, limit)
                    .instrument(tracing::debug_span!("aggregate_response_data"))
                    .await
                    .map_err(|err| {
                        tracing::error!(fetch_error = ?err);
                        let reason = if err.downcast_ref::<LengthLimitError>().is_some() {
                            u64_counter!(
                                "apollo.router.limits.subgraph_response_size.exceeded",
                                "Number of subgraph responses aborted because they exceeded the configured response size limit",
                                1,
                                subgraph.name = service_name.to_string()
                            );
                            tracing::Span::current()
                                .record("apollo.subgraph.response.aborted", "response_size_limit");
                            format!("subgraph response body exceeded limit of {limit} bytes")
                        } else {
                            err.to_string()
                        };
                        FetchError::SubrequestHttpError {
                            status_code: Some(parts.status.as_u16()),
                            service: service_name.to_string(),
                            reason,
                        }
                    })
            }
            None => {
                router::body::into_bytes(body)
                    .instrument(tracing::debug_span!("aggregate_response_data"))
                    .await
                    .map_err(|err| {
                        tracing::error!(fetch_error = ?err);
                        FetchError::SubrequestHttpError {
                            status_code: Some(parts.status.as_u16()),
                            service: service_name.to_string(),
                            reason: err.to_string(),
                        }
                    })
            }
        };
        Some(body_result)
    } else {
        None
    };
    Ok((parts, content_type, body))
}

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;
    use crate::assert_response_eq_ignoring_error_id;

    #[test]
    fn it_gets_uri_details() {
        let path = "https://example.com/path".parse().unwrap();
        let (host, port, path) = super::get_uri_details(&path);

        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/path");
    }

    #[test]
    fn it_converts_ok_http_to_graphql() {
        let (parts, body) = http::Response::builder()
            .status(StatusCode::OK)
            .body(None)
            .unwrap()
            .into_parts();
        let actual = http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder().build();
        assert_eq!(actual, expected);
    }

    #[test]
    fn it_converts_error_http_to_graphql() {
        let (parts, body) = http::Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .body(None)
            .unwrap()
            .into_parts();
        let actual = http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .error(
                super::FetchError::SubrequestHttpError {
                    status_code: Some(418),
                    service: "test_service".into(),
                    reason: "418: I'm a teapot".into(),
                }
                .to_graphql_error(None),
            )
            .build();
        assert_response_eq_ignoring_error_id!(actual, expected);
    }

    #[test]
    fn it_converts_http_with_body_to_graphql() {
        let mut json = serde_json::json!({
            "data": {
                "some_field": "some_value"
            }
        });

        let (parts, body) = http::Response::builder()
            .status(StatusCode::OK)
            .body(Some(Ok(Bytes::from(json.to_string()))))
            .unwrap()
            .into_parts();

        let actual = http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .data(json["data"].take())
            .build();
        assert_eq!(actual, expected);
    }

    #[test]
    fn it_converts_http_with_graphql_errors_to_graphql() {
        let error = graphql::Error::builder()
            .message("error was encountered for test")
            .extension_code("SOME_EXTENSION")
            .extension("service", "test_service")
            .build();
        let mut json = serde_json::json!({
            "data": {
                "some_field": "some_value",
                "error_field": null,
            },
            "errors": [error],
        });

        let (parts, body) = http::Response::builder()
            .status(StatusCode::OK)
            .body(Some(Ok(Bytes::from(json.to_string()))))
            .unwrap()
            .into_parts();

        let actual = http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .data(json["data"].take())
            .error(error)
            .build();
        assert_response_eq_ignoring_error_id!(actual, expected);
    }

    #[test]
    fn it_converts_error_http_with_graphql_errors_to_graphql() {
        let error = graphql::Error::builder()
            .message("error was encountered for test")
            .extension_code("SOME_EXTENSION")
            .extension("service", "test_service")
            .build();
        let mut json = serde_json::json!({
            "data": {
                "some_field": "some_value",
                "error_field": null,
            },
            "errors": [error],
        });

        let (parts, body) = http::Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .body(Some(Ok(Bytes::from(json.to_string()))))
            .unwrap()
            .into_parts();

        let actual = http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .data(json["data"].take())
            .error(
                super::FetchError::SubrequestHttpError {
                    status_code: Some(418),
                    service: "test_service".into(),
                    reason: "418: I'm a teapot".into(),
                }
                .to_graphql_error(None),
            )
            .error(error)
            .build();
        assert_response_eq_ignoring_error_id!(expected, actual);
    }
}
