use std::fmt::Debug;

use http::StatusCode;
use http::header::CONTENT_LENGTH;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::HTTP_REQUEST_BODY_SIZE;
use opentelemetry_semantic_conventions::attribute::HTTP_RESPONSE_BODY_SIZE;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_STATUS_CODE;
use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_NAME;
use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
use opentelemetry_semantic_conventions::trace::NETWORK_TRANSPORT;
use opentelemetry_semantic_conventions::trace::NETWORK_TYPE;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::axum_factory::utils::ConnectionInfo;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::ERROR_TYPE;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;

/// Common attributes for http server and client.
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#common-attributes
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpCommonAttributes {
    /// Describes a class of error the operation ended with.
    /// Examples:
    ///
    /// * timeout
    /// * name_resolution_error
    /// * 500
    ///
    /// Requirement level: Conditionally Required: If request has ended with an error.
    #[serde(rename = "error.type")]
    pub(crate) error_type: Option<StandardAttribute>,

    /// The size of the request payload body in bytes. This is the number of bytes transferred excluding headers and is often, but not always, present as the Content-Length header. For requests using transport encoding, this should be the compressed size.
    /// Examples:
    ///
    /// * 3495
    ///
    /// Requirement level: Recommended
    #[serde(rename = "http.request.body.size")]
    pub(crate) http_request_body_size: Option<StandardAttribute>,

    /// HTTP request method.
    /// Examples:
    ///
    /// * GET
    /// * POST
    /// * HEAD
    ///
    /// Requirement level: Required
    #[serde(rename = "http.request.method")]
    pub(crate) http_request_method: Option<StandardAttribute>,

    /// Original HTTP method sent by the client in the request line.
    /// Examples:
    ///
    /// * GeT
    /// * ACL
    /// * foo
    ///
    /// Requirement level: Conditionally Required (If and only if it’s different than http.request.method)
    #[serde(rename = "http.request.method.original", skip)]
    pub(crate) http_request_method_original: Option<StandardAttribute>,

    /// The size of the response payload body in bytes. This is the number of bytes transferred excluding headers and is often, but not always, present as the Content-Length header. For requests using transport encoding, this should be the compressed size.
    /// Examples:
    ///
    /// * 3495
    ///
    /// Requirement level: Recommended
    #[serde(rename = "http.response.body.size")]
    pub(crate) http_response_body_size: Option<StandardAttribute>,

    /// HTTP response status code.
    /// Examples:
    ///
    /// * 200
    ///
    /// Requirement level: Conditionally Required: If and only if one was received/sent.
    #[serde(rename = "http.response.status_code")]
    pub(crate) http_response_status_code: Option<StandardAttribute>,

    /// OSI application layer or non-OSI equivalent.
    /// Examples:
    ///
    /// * http
    /// * spdy
    ///
    /// Requirement level: Recommended: if not default (http).
    #[serde(rename = "network.protocol.name")]
    pub(crate) network_protocol_name: Option<StandardAttribute>,

    /// Version of the protocol specified in network.protocol.name.
    /// Examples:
    ///
    /// * 1.0
    /// * 1.1
    /// * 2
    /// * 3
    ///
    /// Requirement level: Recommended
    #[serde(rename = "network.protocol.version")]
    pub(crate) network_protocol_version: Option<StandardAttribute>,

    /// OSI transport layer.
    /// Examples:
    ///
    /// * tcp
    /// * udp
    ///
    /// Requirement level: Conditionally Required
    #[serde(rename = "network.transport")]
    pub(crate) network_transport: Option<StandardAttribute>,

    /// OSI network layer or non-OSI equivalent.
    /// Examples:
    ///
    /// * ipv4
    /// * ipv6
    ///
    /// Requirement level: Recommended
    #[serde(rename = "network.type")]
    pub(crate) network_type: Option<StandardAttribute>,
}

impl DefaultForLevel for HttpCommonAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                if self.error_type.is_none() {
                    self.error_type = Some(StandardAttribute::Bool(true));
                }
                if self.http_request_method.is_none() {
                    self.http_request_method = Some(StandardAttribute::Bool(true));
                }
                if self.http_response_status_code.is_none() {
                    self.http_response_status_code = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                // Recommended
                match kind {
                    TelemetryDataKind::Traces => {
                        if self.http_request_body_size.is_none() {
                            self.http_request_body_size = Some(StandardAttribute::Bool(true));
                        }
                        if self.http_response_body_size.is_none() {
                            self.http_response_body_size = Some(StandardAttribute::Bool(true));
                        }
                        if self.network_protocol_version.is_none() {
                            self.network_protocol_version = Some(StandardAttribute::Bool(true));
                        }
                        if self.network_type.is_none() {
                            self.network_type = Some(StandardAttribute::Bool(true));
                        }
                    }
                    TelemetryDataKind::Metrics => {
                        if self.network_protocol_version.is_none() {
                            self.network_protocol_version = Some(StandardAttribute::Bool(true));
                        }
                    }
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors<router::Request, router::Response, ()> for HttpCommonAttributes {
    fn on_request(&self, request: &router::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self
            .http_request_method
            .as_ref()
            .and_then(|a| a.key(HTTP_REQUEST_METHOD.into()))
        {
            attrs.push(KeyValue::new(
                key,
                request.router_request.method().as_str().to_string(),
            ));
        }

        if let Some(key) = self
            .http_request_body_size
            .as_ref()
            .and_then(|a| a.key(HTTP_REQUEST_BODY_SIZE.into()))
            && let Some(content_length) = request
                .router_request
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            && let Ok(content_length) = content_length.parse::<i64>()
        {
            attrs.push(KeyValue::new(
                key,
                opentelemetry::Value::I64(content_length),
            ));
        }
        if let Some(key) = self
            .network_protocol_name
            .as_ref()
            .and_then(|a| a.key(NETWORK_PROTOCOL_NAME.into()))
            && let Some(scheme) = request.router_request.uri().scheme()
        {
            attrs.push(KeyValue::new(key, scheme.to_string()));
        }
        if let Some(key) = self
            .network_protocol_version
            .as_ref()
            .and_then(|a| a.key(NETWORK_PROTOCOL_VERSION.into()))
        {
            attrs.push(KeyValue::new(
                key,
                format!("{:?}", request.router_request.version()),
            ));
        }
        if let Some(key) = self
            .network_transport
            .as_ref()
            .and_then(|a| a.key(NETWORK_TRANSPORT.into()))
        {
            attrs.push(KeyValue::new(key, "tcp".to_string()));
        }
        if let Some(key) = self
            .network_type
            .as_ref()
            .and_then(|a| a.key(NETWORK_TYPE.into()))
            && let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            && let Some(socket) = connection_info.server_address
        {
            if socket.is_ipv4() {
                attrs.push(KeyValue::new(key, "ipv4".to_string()));
            } else if socket.is_ipv6() {
                attrs.push(KeyValue::new(key, "ipv6".to_string()));
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self
            .http_response_body_size
            .as_ref()
            .and_then(|a| a.key(HTTP_RESPONSE_BODY_SIZE.into()))
            && let Some(content_length) = response
                .response
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            && let Ok(content_length) = content_length.parse::<i64>()
        {
            attrs.push(KeyValue::new(
                key,
                opentelemetry::Value::I64(content_length),
            ));
        }

        if let Some(key) = self
            .http_response_status_code
            .as_ref()
            .and_then(|a| a.key(HTTP_RESPONSE_STATUS_CODE.into()))
        {
            attrs.push(KeyValue::new(
                key,
                response.response.status().as_u16() as i64,
            ));
        }

        if let Some(key) = self.error_type.as_ref().and_then(|a| a.key(ERROR_TYPE))
            && !response.response.status().is_success()
        {
            attrs.push(KeyValue::new(
                key,
                response
                    .response
                    .status()
                    .canonical_reason()
                    .unwrap_or("unknown"),
            ));
        }

        attrs
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self.error_type.as_ref().and_then(|a| a.key(ERROR_TYPE)) {
            attrs.push(KeyValue::new(
                key,
                StatusCode::INTERNAL_SERVER_ERROR
                    .canonical_reason()
                    .unwrap_or("unknown"),
            ));
        }
        if let Some(key) = self
            .http_response_status_code
            .as_ref()
            .and_then(|a| a.key(HTTP_RESPONSE_STATUS_CODE.into()))
        {
            attrs.push(KeyValue::new(
                key,
                StatusCode::INTERNAL_SERVER_ERROR.as_u16() as i64,
            ));
        }

        attrs
    }
}

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use anyhow::anyhow;
    use http::HeaderValue;
    use http::StatusCode;
    use http::Uri;
    use opentelemetry_semantic_conventions::attribute::HTTP_REQUEST_BODY_SIZE;
    use opentelemetry_semantic_conventions::attribute::HTTP_RESPONSE_BODY_SIZE;
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
    use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_STATUS_CODE;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_NAME;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
    use opentelemetry_semantic_conventions::trace::NETWORK_TRANSPORT;
    use opentelemetry_semantic_conventions::trace::NETWORK_TYPE;

    use super::*;
    use crate::axum_factory::utils::ConnectionInfo;
    use crate::plugins::telemetry::config_new::attributes::ERROR_TYPE;
    use crate::services::router;

    #[test]
    fn test_http_common_error_type() {
        let common = HttpCommonAttributes {
            error_type: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_response(
            &router::Response::fake_builder()
                .status_code(StatusCode::BAD_REQUEST)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == ERROR_TYPE)
                .map(|key_val| &key_val.value),
            Some(
                &StatusCode::BAD_REQUEST
                    .canonical_reason()
                    .unwrap_or_default()
                    .into()
            )
        );

        let attributes = common.on_error(&anyhow!("test error").into(), &Default::default());
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == ERROR_TYPE)
                .map(|key_val| &key_val.value),
            Some(
                &StatusCode::INTERNAL_SERVER_ERROR
                    .canonical_reason()
                    .unwrap_or_default()
                    .into()
            )
        );
    }

    #[test]
    fn test_http_common_request_body_size() {
        let common = HttpCommonAttributes {
            http_request_body_size: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_request(
            &router::Request::fake_builder()
                .header(
                    http::header::CONTENT_LENGTH,
                    HeaderValue::from_static("256"),
                )
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_REQUEST_BODY_SIZE)
                .map(|key_val| &key_val.value),
            Some(&256.into())
        );
    }

    #[test]
    fn test_http_common_response_body_size() {
        let common = HttpCommonAttributes {
            http_response_body_size: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_response(
            &router::Response::fake_builder()
                .header(
                    http::header::CONTENT_LENGTH,
                    HeaderValue::from_static("256"),
                )
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_RESPONSE_BODY_SIZE)
                .map(|key_val| &key_val.value),
            Some(&256.into())
        );
    }

    #[test]
    fn test_http_common_request_method() {
        let common = HttpCommonAttributes {
            http_request_method: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_request(
            &router::Request::fake_builder()
                .method(http::Method::POST)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_REQUEST_METHOD)
                .map(|key_val| &key_val.value),
            Some(&"POST".into())
        );
    }

    #[test]
    fn test_http_common_response_status_code() {
        let common = HttpCommonAttributes {
            http_response_status_code: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_response(
            &router::Response::fake_builder()
                .status_code(StatusCode::BAD_REQUEST)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_RESPONSE_STATUS_CODE)
                .map(|key_val| &key_val.value),
            Some(&(StatusCode::BAD_REQUEST.as_u16() as i64).into())
        );

        let attributes = common.on_error(&anyhow!("test error").into(), &Default::default());
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == HTTP_RESPONSE_STATUS_CODE)
                .map(|key_val| &key_val.value),
            Some(&(StatusCode::INTERNAL_SERVER_ERROR.as_u16() as i64).into())
        );
    }

    #[test]
    fn test_http_common_network_protocol_name() {
        let common = HttpCommonAttributes {
            network_protocol_name: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_request(
            &router::Request::fake_builder()
                .uri(Uri::from_static("https://localhost/graphql"))
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == NETWORK_PROTOCOL_NAME)
                .map(|key_val| &key_val.value),
            Some(&"https".into())
        );
    }

    #[test]
    fn test_http_common_network_protocol_version() {
        let common = HttpCommonAttributes {
            network_protocol_version: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_request(
            &router::Request::fake_builder()
                .uri(Uri::from_static("https://localhost/graphql"))
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == NETWORK_PROTOCOL_VERSION)
                .map(|key_val| &key_val.value),
            Some(&"HTTP/1.1".into())
        );
    }

    #[test]
    fn test_http_common_network_transport() {
        let common = HttpCommonAttributes {
            network_transport: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = common.on_request(&router::Request::fake_builder().build().unwrap());
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == NETWORK_TRANSPORT)
                .map(|key_val| &key_val.value),
            Some(&"tcp".into())
        );
    }

    #[test]
    fn test_http_common_network_type() {
        let common = HttpCommonAttributes {
            network_type: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder().build().unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = common.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == NETWORK_TYPE)
                .map(|key_val| &key_val.value),
            Some(&"ipv4".into())
        );
    }
}
