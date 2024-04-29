use std::fmt::Debug;
use std::net::SocketAddr;

use http::header::CONTENT_LENGTH;
use http::header::FORWARDED;
use http::header::USER_AGENT;
use http::StatusCode;
use http::Uri;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry_api::baggage::BaggageExt;
use opentelemetry_semantic_conventions::trace::CLIENT_ADDRESS;
use opentelemetry_semantic_conventions::trace::CLIENT_PORT;
use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_BODY_SIZE;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_BODY_SIZE;
use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_STATUS_CODE;
use opentelemetry_semantic_conventions::trace::HTTP_ROUTE;
use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_NAME;
use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
use opentelemetry_semantic_conventions::trace::NETWORK_TRANSPORT;
use opentelemetry_semantic_conventions::trace::NETWORK_TYPE;
use opentelemetry_semantic_conventions::trace::SERVER_ADDRESS;
use opentelemetry_semantic_conventions::trace::SERVER_PORT;
use opentelemetry_semantic_conventions::trace::URL_PATH;
use opentelemetry_semantic_conventions::trace::URL_QUERY;
use opentelemetry_semantic_conventions::trace::URL_SCHEME;
use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;
use schemars::JsonSchema;
use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use tower::BoxError;
use tracing::Span;

use crate::axum_factory::utils::ConnectionInfo;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;
use crate::services::router::Request;
use crate::services::subgraph;
use crate::services::supergraph;

pub(crate) const SUBGRAPH_NAME: Key = Key::from_static_str("subgraph.name");
pub(crate) const SUBGRAPH_GRAPHQL_DOCUMENT: Key = Key::from_static_str("subgraph.graphql.document");
pub(crate) const SUBGRAPH_GRAPHQL_OPERATION_NAME: Key =
    Key::from_static_str("subgraph.graphql.operation.name");
pub(crate) const SUBGRAPH_GRAPHQL_OPERATION_TYPE: Key =
    Key::from_static_str("subgraph.graphql.operation.type");

const ERROR_TYPE: Key = Key::from_static_str("error.type");

const NETWORK_LOCAL_ADDRESS: Key = Key::from_static_str("network.local.address");
const NETWORK_LOCAL_PORT: Key = Key::from_static_str("network.local.port");

const NETWORK_PEER_ADDRESS: Key = Key::from_static_str("network.peer.address");
const NETWORK_PEER_PORT: Key = Key::from_static_str("network.peer.port");

#[derive(Deserialize, JsonSchema, Clone, Debug, Default, Copy)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum DefaultAttributeRequirementLevel {
    /// No default attributes set on spans, you have to set it one by one in the configuration to enable some attributes
    None,
    /// Attributes that are marked as required in otel semantic conventions and apollo documentation will be included (default)
    #[default]
    Required,
    /// Attributes that are marked as required or recommended in otel semantic conventions and apollo documentation will be included
    Recommended,
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterAttributes {
    /// The datadog trace ID.
    /// This can be output in logs and used to correlate traces in Datadog.
    #[serde(rename = "dd.trace_id")]
    pub(crate) datadog_trace_id: Option<bool>,

    /// The OpenTelemetry trace ID.
    /// This can be output in logs.
    #[serde(rename = "trace_id")]
    pub(crate) trace_id: Option<bool>,

    /// All key values from trace baggage.
    pub(crate) baggage: Option<bool>,

    /// Http attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    pub(crate) common: HttpCommonAttributes,
    /// Http server attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    pub(crate) server: HttpServerAttributes,
}

impl DefaultForLevel for RouterAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        self.common.defaults_for_level(requirement_level, kind);
        self.server.defaults_for_level(requirement_level, kind);
    }
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(Serialize, PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphAttributes {
    /// The GraphQL document being executed.
    /// Examples:
    /// * query findBookById { bookById(id: ?) { name } }
    /// Requirement level: Recommended
    #[serde(rename = "graphql.document")]
    pub(crate) graphql_document: Option<bool>,
    /// The name of the operation being executed.
    /// Examples:
    /// * findBookById
    /// Requirement level: Recommended
    #[serde(rename = "graphql.operation.name")]
    pub(crate) graphql_operation_name: Option<bool>,
    /// The type of the operation being executed.
    /// Examples:
    /// * query
    /// * subscription
    /// * mutation
    /// Requirement level: Recommended
    #[serde(rename = "graphql.operation.type")]
    pub(crate) graphql_operation_type: Option<bool>,
}

impl DefaultForLevel for SupergraphAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {}
            DefaultAttributeRequirementLevel::Recommended => {
                if self.graphql_document.is_none() {
                    self.graphql_document = Some(true);
                }
                if self.graphql_operation_name.is_none() {
                    self.graphql_operation_name = Some(true);
                }
                if self.graphql_operation_type.is_none() {
                    self.graphql_operation_type = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphAttributes {
    /// The name of the subgraph
    /// Examples:
    /// * products
    /// Requirement level: Required
    #[serde(rename = "subgraph.name")]
    subgraph_name: Option<bool>,
    /// The GraphQL document being executed.
    /// Examples:
    /// * query findBookById { bookById(id: ?) { name } }
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.document")]
    graphql_document: Option<bool>,
    /// The name of the operation being executed.
    /// Examples:
    /// * findBookById
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.name")]
    graphql_operation_name: Option<bool>,
    /// The type of the operation being executed.
    /// Examples:
    /// * query
    /// * subscription
    /// * mutation
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.type")]
    graphql_operation_type: Option<bool>,
}

impl DefaultForLevel for SubgraphAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                if self.subgraph_name.is_none() {
                    self.subgraph_name = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                if self.subgraph_name.is_none() {
                    self.subgraph_name = Some(true);
                }
                if self.graphql_document.is_none() {
                    self.graphql_document = Some(true);
                }
                if self.graphql_operation_name.is_none() {
                    self.graphql_operation_name = Some(true);
                }
                if self.graphql_operation_type.is_none() {
                    self.graphql_operation_type = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

/// Common attributes for http server and client.
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#common-attributes
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpCommonAttributes {
    /// Describes a class of error the operation ended with.
    /// Examples:
    /// * timeout
    /// * name_resolution_error
    /// * 500
    /// Requirement level: Conditionally Required: If request has ended with an error.
    #[serde(rename = "error.type")]
    pub(crate) error_type: Option<bool>,

    /// The size of the request payload body in bytes. This is the number of bytes transferred excluding headers and is often, but not always, present as the Content-Length header. For requests using transport encoding, this should be the compressed size.
    /// Examples:
    /// * 3495
    /// Requirement level: Recommended
    #[serde(rename = "http.request.body.size")]
    pub(crate) http_request_body_size: Option<bool>,

    /// HTTP request method.
    /// Examples:
    /// * GET
    /// * POST
    /// * HEAD
    /// Requirement level: Required
    #[serde(rename = "http.request.method")]
    pub(crate) http_request_method: Option<bool>,

    /// Original HTTP method sent by the client in the request line.
    /// Examples:
    /// * GeT
    /// * ACL
    /// * foo
    /// Requirement level: Conditionally Required (If and only if it’s different than http.request.method)
    #[serde(rename = "http.request.method.original", skip)]
    pub(crate) http_request_method_original: Option<bool>,

    /// The size of the response payload body in bytes. This is the number of bytes transferred excluding headers and is often, but not always, present as the Content-Length header. For requests using transport encoding, this should be the compressed size.
    /// Examples:
    /// * 3495
    /// Requirement level: Recommended
    #[serde(rename = "http.response.body.size")]
    pub(crate) http_response_body_size: Option<bool>,

    /// HTTP response status code.
    /// Examples:
    /// * 200
    /// Requirement level: Conditionally Required: If and only if one was received/sent.
    #[serde(rename = "http.response.status_code")]
    pub(crate) http_response_status_code: Option<bool>,

    /// OSI application layer or non-OSI equivalent.
    /// Examples:
    /// * http
    /// * spdy
    /// Requirement level: Recommended: if not default (http).
    #[serde(rename = "network.protocol.name")]
    pub(crate) network_protocol_name: Option<bool>,

    /// Version of the protocol specified in network.protocol.name.
    /// Examples:
    /// * 1.0
    /// * 1.1
    /// * 2
    /// * 3
    /// Requirement level: Recommended
    #[serde(rename = "network.protocol.version")]
    pub(crate) network_protocol_version: Option<bool>,

    /// OSI transport layer.
    /// Examples:
    /// * tcp
    /// * udp
    /// Requirement level: Conditionally Required
    #[serde(rename = "network.transport")]
    pub(crate) network_transport: Option<bool>,

    /// OSI network layer or non-OSI equivalent.
    /// Examples:
    /// * ipv4
    /// * ipv6
    /// Requirement level: Recommended
    #[serde(rename = "network.type")]
    pub(crate) network_type: Option<bool>,
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
                    self.error_type = Some(true);
                }
                if self.http_request_method.is_none() {
                    self.http_request_method = Some(true);
                }
                if self.http_response_status_code.is_none() {
                    self.http_response_status_code = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                // Recommended
                match kind {
                    TelemetryDataKind::Traces => {
                        if self.http_request_body_size.is_none() {
                            self.http_request_body_size = Some(true);
                        }
                        if self.http_response_body_size.is_none() {
                            self.http_response_body_size = Some(true);
                        }
                        if self.network_protocol_version.is_none() {
                            self.network_protocol_version = Some(true);
                        }
                        if self.network_type.is_none() {
                            self.network_type = Some(true);
                        }
                    }
                    TelemetryDataKind::Metrics => {
                        if self.network_protocol_version.is_none() {
                            self.network_protocol_version = Some(true);
                        }
                    }
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

/// Attributes for Http servers
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-server
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpServerAttributes {
    /// Client address - domain name if available without reverse DNS lookup, otherwise IP address or Unix domain socket name.
    /// Examples:
    /// * 83.164.160.102
    /// Requirement level: Recommended
    #[serde(rename = "client.address", skip)]
    pub(crate) client_address: Option<bool>,
    /// The port of the original client behind all proxies, if known (e.g. from Forwarded or a similar header). Otherwise, the immediate client peer port.
    /// Examples:
    /// * 65123
    /// Requirement level: Recommended
    #[serde(rename = "client.port", skip)]
    pub(crate) client_port: Option<bool>,
    /// The matched route (path template in the format used by the respective server framework).
    /// Examples:
    /// * /graphql
    /// Requirement level: Conditionally Required: If and only if it’s available
    #[serde(rename = "http.route")]
    pub(crate) http_route: Option<bool>,
    /// Local socket address. Useful in case of a multi-IP host.
    /// Examples:
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.address")]
    pub(crate) network_local_address: Option<bool>,
    /// Local socket port. Useful in case of a multi-port host.
    /// Examples:
    /// * 65123
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.port")]
    pub(crate) network_local_port: Option<bool>,
    /// Peer address of the network connection - IP address or Unix domain socket name.
    /// Examples:
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.address")]
    pub(crate) network_peer_address: Option<bool>,
    /// Peer port number of the network connection.
    /// Examples:
    /// * 65123
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.port")]
    pub(crate) network_peer_port: Option<bool>,
    /// Name of the local HTTP server that received the request.
    /// Examples:
    /// * example.com
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Recommended
    #[serde(rename = "server.address")]
    pub(crate) server_address: Option<bool>,
    /// Port of the local HTTP server that received the request.
    /// Examples:
    /// * 80
    /// * 8080
    /// * 443
    /// Requirement level: Recommended
    #[serde(rename = "server.port")]
    pub(crate) server_port: Option<bool>,
    /// The URI path component
    /// Examples:
    /// * /search
    /// Requirement level: Required
    #[serde(rename = "url.path")]
    pub(crate) url_path: Option<bool>,
    /// The URI query component
    /// Examples:
    /// * q=OpenTelemetry
    /// Requirement level: Conditionally Required: If and only if one was received/sent.
    #[serde(rename = "url.query")]
    pub(crate) url_query: Option<bool>,

    /// The URI scheme component identifying the used protocol.
    /// Examples:
    /// * http
    /// * https
    /// Requirement level: Required
    #[serde(rename = "url.scheme")]
    pub(crate) url_scheme: Option<bool>,

    /// Value of the HTTP User-Agent header sent by the client.
    /// Examples:
    /// * CERN-LineMode/2.15
    /// * libwww/2.17b3
    /// Requirement level: Recommended
    #[serde(rename = "user_agent.original")]
    pub(crate) user_agent_original: Option<bool>,
}

impl DefaultForLevel for HttpServerAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => match kind {
                TelemetryDataKind::Traces => {
                    if self.url_scheme.is_none() {
                        self.url_scheme = Some(true);
                    }
                    if self.url_path.is_none() {
                        self.url_path = Some(true);
                    }
                    if self.url_query.is_none() {
                        self.url_query = Some(true);
                    }

                    if self.http_route.is_none() {
                        self.http_route = Some(true);
                    }
                }
                TelemetryDataKind::Metrics => {
                    if self.server_address.is_none() {
                        self.server_address = Some(true);
                    }
                    if self.server_port.is_none() && self.server_address.is_some() {
                        self.server_port = Some(true);
                    }
                }
            },
            DefaultAttributeRequirementLevel::Recommended => match kind {
                TelemetryDataKind::Traces => {
                    if self.client_address.is_none() {
                        self.client_address = Some(true);
                    }
                    if self.server_address.is_none() {
                        self.server_address = Some(true);
                    }
                    if self.server_port.is_none() && self.server_address.is_some() {
                        self.server_port = Some(true);
                    }
                    if self.user_agent_original.is_none() {
                        self.user_agent_original = Some(true);
                    }
                }
                TelemetryDataKind::Metrics => {}
            },
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors for RouterAttributes {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> Vec<KeyValue> {
        let mut attrs = self.common.on_request(request);
        attrs.extend(self.server.on_request(request));
        if let Some(true) = &self.trace_id {
            if let Some(trace_id) = trace_id() {
                attrs.push(KeyValue::new(
                    Key::from_static_str("trace_id"),
                    trace_id.to_string(),
                ));
            }
        }
        if let Some(true) = &self.datadog_trace_id {
            if let Some(trace_id) = trace_id() {
                attrs.push(KeyValue::new(
                    Key::from_static_str("dd.trace_id"),
                    trace_id.to_datadog(),
                ));
            }
        }
        if let Some(true) = &self.baggage {
            let context = Span::current().context();
            let baggage = context.baggage();
            for (key, (value, _)) in baggage {
                attrs.push(KeyValue::new(key.clone(), value.clone()));
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> Vec<KeyValue> {
        let mut attrs = self.common.on_response(response);
        attrs.extend(self.server.on_response(response));
        attrs
    }

    fn on_error(&self, error: &BoxError) -> Vec<KeyValue> {
        let mut attrs = self.common.on_error(error);
        attrs.extend(self.server.on_error(error));
        attrs
    }
}

impl Selectors for HttpCommonAttributes {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(true) = &self.http_request_method {
            attrs.push(KeyValue::new(
                HTTP_REQUEST_METHOD,
                request.router_request.method().as_str().to_string(),
            ));
        }

        if let Some(true) = &self.http_request_body_size {
            if let Some(content_length) = request
                .router_request
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            {
                attrs.push(KeyValue::new(
                    HTTP_REQUEST_BODY_SIZE,
                    content_length.to_string(),
                ));
            }
        }
        if let Some(true) = &self.network_protocol_name {
            if let Some(scheme) = request.router_request.uri().scheme() {
                attrs.push(KeyValue::new(NETWORK_PROTOCOL_NAME, scheme.to_string()));
            }
        }
        if let Some(true) = &self.network_protocol_version {
            attrs.push(KeyValue::new(
                NETWORK_PROTOCOL_VERSION,
                format!("{:?}", request.router_request.version()),
            ));
        }
        if let Some(true) = &self.network_transport {
            attrs.push(KeyValue::new(NETWORK_TRANSPORT, "tcp".to_string()));
        }
        if let Some(true) = &self.network_type {
            if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.server_address {
                    if socket.is_ipv4() {
                        attrs.push(KeyValue::new(NETWORK_TYPE, "ipv4".to_string()));
                    } else if socket.is_ipv6() {
                        attrs.push(KeyValue::new(NETWORK_TYPE, "ipv6".to_string()));
                    }
                }
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(true) = &self.http_response_body_size {
            if let Some(content_length) = response
                .response
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            {
                attrs.push(KeyValue::new(
                    HTTP_RESPONSE_BODY_SIZE,
                    content_length.to_string(),
                ));
            }
        }

        if let Some(true) = &self.http_response_status_code {
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_STATUS_CODE,
                response.response.status().as_u16() as i64,
            ));
        }

        if let Some(true) = &self.error_type {
            if !response.response.status().is_success() {
                attrs.push(KeyValue::new(
                    ERROR_TYPE,
                    response
                        .response
                        .status()
                        .canonical_reason()
                        .unwrap_or("unknown"),
                ));
            }
        }

        attrs
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(true) = &self.error_type {
            attrs.push(KeyValue::new(
                ERROR_TYPE,
                StatusCode::INTERNAL_SERVER_ERROR
                    .canonical_reason()
                    .unwrap_or("unknown"),
            ));
        }
        if let Some(true) = &self.http_response_status_code {
            attrs.push(KeyValue::new(
                HTTP_RESPONSE_STATUS_CODE,
                StatusCode::INTERNAL_SERVER_ERROR.as_u16() as i64,
            ));
        }

        attrs
    }
}

impl Selectors for HttpServerAttributes {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(true) = &self.http_route {
            attrs.push(KeyValue::new(
                HTTP_ROUTE,
                request.router_request.uri().path().to_string(),
            ));
        }
        if let Some(true) = &self.client_address {
            if let Some(forwarded) = Self::forwarded_for(request) {
                attrs.push(KeyValue::new(CLIENT_ADDRESS, forwarded.ip().to_string()));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.peer_address {
                    attrs.push(KeyValue::new(CLIENT_ADDRESS, socket.ip().to_string()));
                }
            }
        }
        if let Some(true) = &self.client_port {
            if let Some(forwarded) = Self::forwarded_for(request) {
                attrs.push(KeyValue::new(CLIENT_PORT, forwarded.port() as i64));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.peer_address {
                    attrs.push(KeyValue::new(CLIENT_PORT, socket.port() as i64));
                }
            }
        }

        if let Some(true) = &self.server_address {
            if let Some(forwarded) =
                Self::forwarded_host(request).and_then(|h| h.host().map(|h| h.to_string()))
            {
                attrs.push(KeyValue::new(SERVER_ADDRESS, forwarded));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.server_address {
                    attrs.push(KeyValue::new(SERVER_ADDRESS, socket.ip().to_string()));
                }
            }
        }
        if let Some(true) = &self.server_port {
            if let Some(forwarded) = Self::forwarded_host(request).and_then(|h| h.port_u16()) {
                attrs.push(KeyValue::new(SERVER_PORT, forwarded as i64));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.server_address {
                    attrs.push(KeyValue::new(SERVER_PORT, socket.port() as i64));
                }
            }
        }

        if let Some(true) = &self.network_local_address {
            if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.server_address {
                    attrs.push(KeyValue::new(
                        NETWORK_LOCAL_ADDRESS,
                        socket.ip().to_string(),
                    ));
                }
            }
        }
        if let Some(true) = &self.network_local_port {
            if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.server_address {
                    attrs.push(KeyValue::new(NETWORK_LOCAL_PORT, socket.port() as i64));
                }
            }
        }

        if let Some(true) = &self.network_peer_address {
            if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.peer_address {
                    attrs.push(KeyValue::new(NETWORK_PEER_ADDRESS, socket.ip().to_string()));
                }
            }
        }
        if let Some(true) = &self.network_peer_port {
            if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            {
                if let Some(socket) = connection_info.peer_address {
                    attrs.push(KeyValue::new(NETWORK_PEER_PORT, socket.port() as i64));
                }
            }
        }

        let router_uri = request.router_request.uri();
        if let Some(true) = &self.url_path {
            attrs.push(KeyValue::new(URL_PATH, router_uri.path().to_string()));
        }
        if let Some(true) = &self.url_query {
            if let Some(query) = router_uri.query() {
                attrs.push(KeyValue::new(URL_QUERY, query.to_string()));
            }
        }
        if let Some(true) = &self.url_scheme {
            if let Some(scheme) = router_uri.scheme_str() {
                attrs.push(KeyValue::new(URL_SCHEME, scheme.to_string()));
            }
        }
        if let Some(true) = &self.user_agent_original {
            if let Some(user_agent) = request
                .router_request
                .headers()
                .get(&USER_AGENT)
                .and_then(|h| h.to_str().ok())
            {
                attrs.push(KeyValue::new(USER_AGENT_ORIGINAL, user_agent.to_string()));
            }
        }

        attrs
    }

    fn on_response(&self, _response: &router::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }
}

impl HttpServerAttributes {
    fn forwarded_for(request: &Request) -> Option<SocketAddr> {
        request
            .router_request
            .headers()
            .get_all(FORWARDED)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .filter_map(|h| {
                if h.to_lowercase().starts_with("for=") {
                    Some(&h[4..])
                } else {
                    None
                }
            })
            .filter_map(|forwarded| forwarded.parse::<SocketAddr>().ok())
            .next()
    }

    pub(crate) fn forwarded_host(request: &Request) -> Option<Uri> {
        request
            .router_request
            .headers()
            .get_all(FORWARDED)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .filter_map(|h| {
                if h.to_lowercase().starts_with("host=") {
                    Some(&h[5..])
                } else {
                    None
                }
            })
            .filter_map(|forwarded| forwarded.parse::<Uri>().ok())
            .next()
    }
}

impl Selectors for SupergraphAttributes {
    type Request = supergraph::Request;
    type Response = supergraph::Response;

    fn on_request(&self, request: &supergraph::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(true) = &self.graphql_document {
            if let Some(query) = &request.supergraph_request.body().query {
                attrs.push(KeyValue::new(GRAPHQL_DOCUMENT, query.clone()));
            }
        }
        if let Some(true) = &self.graphql_operation_name {
            if let Some(operation_name) = &request
                .context
                .get::<_, String>(OPERATION_NAME)
                .unwrap_or_default()
            {
                attrs.push(KeyValue::new(
                    GRAPHQL_OPERATION_NAME,
                    operation_name.clone(),
                ));
            }
        }
        if let Some(true) = &self.graphql_operation_type {
            if let Some(operation_type) = &request
                .context
                .get::<_, String>(OPERATION_KIND)
                .unwrap_or_default()
            {
                attrs.push(KeyValue::new(
                    GRAPHQL_OPERATION_TYPE,
                    operation_type.clone(),
                ));
            }
        }

        attrs
    }

    fn on_response(&self, _response: &supergraph::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }
}

impl Selectors for SubgraphAttributes {
    type Request = subgraph::Request;
    type Response = subgraph::Response;

    fn on_request(&self, request: &subgraph::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(true) = &self.graphql_document {
            if let Some(query) = &request.subgraph_request.body().query {
                attrs.push(KeyValue::new(SUBGRAPH_GRAPHQL_DOCUMENT, query.clone()));
            }
        }
        if let Some(true) = &self.graphql_operation_name {
            if let Some(op_name) = &request.subgraph_request.body().operation_name {
                attrs.push(KeyValue::new(
                    SUBGRAPH_GRAPHQL_OPERATION_NAME,
                    op_name.clone(),
                ));
            }
        }
        if let Some(true) = &self.graphql_operation_type {
            // Subgraph operation type wil always match the supergraph operation type
            if let Some(operation_type) = &request
                .context
                .get::<_, String>(OPERATION_KIND)
                .unwrap_or_default()
            {
                attrs.push(KeyValue::new(
                    SUBGRAPH_GRAPHQL_OPERATION_TYPE,
                    operation_type.clone(),
                ));
            }
        }
        if let Some(true) = &self.subgraph_name {
            if let Some(subgraph_name) = &request.subgraph_name {
                attrs.push(KeyValue::new(SUBGRAPH_NAME, subgraph_name.clone()));
            }
        }

        attrs
    }

    fn on_response(&self, _response: &subgraph::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError) -> Vec<KeyValue> {
        Vec::default()
    }
}

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use anyhow::anyhow;
    use http::header::FORWARDED;
    use http::header::USER_AGENT;
    use http::HeaderValue;
    use http::StatusCode;
    use http::Uri;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry::Context;
    use opentelemetry_api::baggage::BaggageExt;
    use opentelemetry_api::KeyValue;
    use opentelemetry_semantic_conventions::trace::CLIENT_ADDRESS;
    use opentelemetry_semantic_conventions::trace::CLIENT_PORT;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_BODY_SIZE;
    use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
    use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_BODY_SIZE;
    use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_STATUS_CODE;
    use opentelemetry_semantic_conventions::trace::HTTP_ROUTE;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_NAME;
    use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
    use opentelemetry_semantic_conventions::trace::NETWORK_TRANSPORT;
    use opentelemetry_semantic_conventions::trace::NETWORK_TYPE;
    use opentelemetry_semantic_conventions::trace::SERVER_ADDRESS;
    use opentelemetry_semantic_conventions::trace::SERVER_PORT;
    use opentelemetry_semantic_conventions::trace::URL_PATH;
    use opentelemetry_semantic_conventions::trace::URL_QUERY;
    use opentelemetry_semantic_conventions::trace::URL_SCHEME;
    use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;
    use tracing::span;
    use tracing::subscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::axum_factory::utils::ConnectionInfo;
    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::graphql;
    use crate::plugins::telemetry::config_new::attributes::HttpCommonAttributes;
    use crate::plugins::telemetry::config_new::attributes::HttpServerAttributes;
    use crate::plugins::telemetry::config_new::attributes::RouterAttributes;
    use crate::plugins::telemetry::config_new::attributes::SubgraphAttributes;
    use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
    use crate::plugins::telemetry::config_new::attributes::ERROR_TYPE;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_LOCAL_ADDRESS;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_LOCAL_PORT;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_PEER_ADDRESS;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_PEER_PORT;
    use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_GRAPHQL_DOCUMENT;
    use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_GRAPHQL_OPERATION_NAME;
    use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_GRAPHQL_OPERATION_TYPE;
    use crate::plugins::telemetry::config_new::attributes::SUBGRAPH_NAME;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::otel;
    use crate::services::router;
    use crate::services::subgraph;
    use crate::services::supergraph;

    #[test]
    fn test_router_trace_attributes() {
        let subscriber = tracing_subscriber::registry().with(otel::layer());
        subscriber::with_default(subscriber, || {
            let span_context = SpanContext::new(
                TraceId::from_u128(42),
                SpanId::from_u64(42),
                TraceFlags::default(),
                false,
                TraceState::default(),
            );
            let _context = Context::current()
                .with_remote_span_context(span_context)
                .with_baggage(vec![
                    KeyValue::new("baggage_key", "baggage_value"),
                    KeyValue::new("baggage_key_bis", "baggage_value_bis"),
                ])
                .attach();
            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();

            let attributes = RouterAttributes {
                datadog_trace_id: Some(true),
                trace_id: Some(true),
                baggage: Some(true),
                common: Default::default(),
                server: Default::default(),
            };
            let attributes =
                attributes.on_request(&router::Request::fake_builder().build().unwrap());

            assert_eq!(
                attributes
                    .iter()
                    .find(|key_val| key_val.key == opentelemetry::Key::from_static_str("trace_id"))
                    .map(|key_val| &key_val.value),
                Some(&"0000000000000000000000000000002a".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(
                        |key_val| key_val.key == opentelemetry::Key::from_static_str("dd.trace_id")
                    )
                    .map(|key_val| &key_val.value),
                Some(&"42".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(
                        |key_val| key_val.key == opentelemetry::Key::from_static_str("baggage_key")
                    )
                    .map(|key_val| &key_val.value),
                Some(&"baggage_value".into())
            );
            assert_eq!(
                attributes
                    .iter()
                    .find(|key_val| key_val.key
                        == opentelemetry::Key::from_static_str("baggage_key_bis"))
                    .map(|key_val| &key_val.value),
                Some(&"baggage_value_bis".into())
            );
        });
    }

    #[test]
    fn test_supergraph_graphql_document() {
        let attributes = SupergraphAttributes {
            graphql_document: Some(true),
            ..Default::default()
        };
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == GRAPHQL_DOCUMENT)
                .map(|key_val| &key_val.value),
            Some(&"query { __typename }".into())
        );
    }

    #[test]
    fn test_supergraph_graphql_operation_name() {
        let attributes = SupergraphAttributes {
            graphql_operation_name: Some(true),
            ..Default::default()
        };
        let context = crate::Context::new();
        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .context(context)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == GRAPHQL_OPERATION_NAME)
                .map(|key_val| &key_val.value),
            Some(&"topProducts".into())
        );
    }

    #[test]
    fn test_supergraph_graphql_operation_type() {
        let attributes = SupergraphAttributes {
            graphql_operation_type: Some(true),
            ..Default::default()
        };
        let context = crate::Context::new();
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .context(context)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == GRAPHQL_OPERATION_TYPE)
                .map(|key_val| &key_val.value),
            Some(&"query".into())
        );
    }

    #[test]
    fn test_subgraph_graphql_document() {
        let attributes = SubgraphAttributes {
            graphql_document: Some(true),
            ..Default::default()
        };
        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .query("query { __typename }")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_GRAPHQL_DOCUMENT)
                .map(|key_val| &key_val.value),
            Some(&"query { __typename }".into())
        );
    }

    #[test]
    fn test_subgraph_graphql_operation_name() {
        let attributes = SubgraphAttributes {
            graphql_operation_name: Some(true),
            ..Default::default()
        };

        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .operation_name("topProducts")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_GRAPHQL_OPERATION_NAME)
                .map(|key_val| &key_val.value),
            Some(&"topProducts".into())
        );
    }

    #[test]
    fn test_subgraph_graphql_operation_type() {
        let attributes = SubgraphAttributes {
            graphql_operation_type: Some(true),
            ..Default::default()
        };

        let context = crate::Context::new();
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .context(context)
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(graphql::Request::fake_builder().build())
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_GRAPHQL_OPERATION_TYPE)
                .map(|key_val| &key_val.value),
            Some(&"query".into())
        );
    }

    #[test]
    fn test_subgraph_name() {
        let attributes = SubgraphAttributes {
            subgraph_name: Some(true),
            ..Default::default()
        };

        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_name("products")
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(graphql::Request::fake_builder().build())
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_NAME)
                .map(|key_val| &key_val.value),
            Some(&"products".into())
        );
    }

    #[test]
    fn test_http_common_error_type() {
        let common = HttpCommonAttributes {
            error_type: Some(true),
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

        let attributes = common.on_error(&anyhow!("test error").into());
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
            http_request_body_size: Some(true),
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
                .find(|key_val| key_val.key == HTTP_REQUEST_BODY_SIZE)
                .map(|key_val| &key_val.value),
            Some(&"256".into())
        );
    }

    #[test]
    fn test_http_common_response_body_size() {
        let common = HttpCommonAttributes {
            http_response_body_size: Some(true),
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
                .find(|key_val| key_val.key == HTTP_RESPONSE_BODY_SIZE)
                .map(|key_val| &key_val.value),
            Some(&"256".into())
        );
    }

    #[test]
    fn test_http_common_request_method() {
        let common = HttpCommonAttributes {
            http_request_method: Some(true),
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
                .find(|key_val| key_val.key == HTTP_REQUEST_METHOD)
                .map(|key_val| &key_val.value),
            Some(&"POST".into())
        );
    }

    #[test]
    fn test_http_common_response_status_code() {
        let common = HttpCommonAttributes {
            http_response_status_code: Some(true),
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
                .find(|key_val| key_val.key == HTTP_RESPONSE_STATUS_CODE)
                .map(|key_val| &key_val.value),
            Some(&(StatusCode::BAD_REQUEST.as_u16() as i64).into())
        );

        let attributes = common.on_error(&anyhow!("test error").into());
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == HTTP_RESPONSE_STATUS_CODE)
                .map(|key_val| &key_val.value),
            Some(&(StatusCode::INTERNAL_SERVER_ERROR.as_u16() as i64).into())
        );
    }

    #[test]
    fn test_http_common_network_protocol_name() {
        let common = HttpCommonAttributes {
            network_protocol_name: Some(true),
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
                .find(|key_val| key_val.key == NETWORK_PROTOCOL_NAME)
                .map(|key_val| &key_val.value),
            Some(&"https".into())
        );
    }

    #[test]
    fn test_http_common_network_protocol_version() {
        let common = HttpCommonAttributes {
            network_protocol_version: Some(true),
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
                .find(|key_val| key_val.key == NETWORK_PROTOCOL_VERSION)
                .map(|key_val| &key_val.value),
            Some(&"HTTP/1.1".into())
        );
    }

    #[test]
    fn test_http_common_network_transport() {
        let common = HttpCommonAttributes {
            network_transport: Some(true),
            ..Default::default()
        };

        let attributes = common.on_request(&router::Request::fake_builder().build().unwrap());
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == NETWORK_TRANSPORT)
                .map(|key_val| &key_val.value),
            Some(&"tcp".into())
        );
    }

    #[test]
    fn test_http_common_network_type() {
        let common = HttpCommonAttributes {
            network_type: Some(true),
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
                .find(|key_val| key_val.key == NETWORK_TYPE)
                .map(|key_val| &key_val.value),
            Some(&"ipv4".into())
        );
    }

    #[test]
    fn test_http_server_client_address() {
        let server = HttpServerAttributes {
            client_address: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder().build().unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == CLIENT_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"192.168.0.8".into())
        );

        let mut req = router::Request::fake_builder()
            .header(FORWARDED, "for=2.4.6.8:8000")
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == CLIENT_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"2.4.6.8".into())
        );
    }

    #[test]
    fn test_http_server_client_port() {
        let server = HttpServerAttributes {
            client_port: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder().build().unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == CLIENT_PORT)
                .map(|key_val| &key_val.value),
            Some(&6060.into())
        );

        let mut req = router::Request::fake_builder()
            .header(FORWARDED, "for=2.4.6.8:8000")
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == CLIENT_PORT)
                .map(|key_val| &key_val.value),
            Some(&8000.into())
        );
    }

    #[test]
    fn test_http_server_http_route() {
        let server = HttpServerAttributes {
            http_route: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder()
            .uri(Uri::from_static("https://localhost/graphql"))
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == HTTP_ROUTE)
                .map(|key_val| &key_val.value),
            Some(&"/graphql".into())
        );
    }

    #[test]
    fn test_http_server_network_local_address() {
        let server = HttpServerAttributes {
            network_local_address: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder()
            .uri(Uri::from_static("https://localhost/graphql"))
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == NETWORK_LOCAL_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"192.168.0.1".into())
        );
    }

    #[test]
    fn test_http_server_network_local_port() {
        let server = HttpServerAttributes {
            network_local_port: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder()
            .uri(Uri::from_static("https://localhost/graphql"))
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == NETWORK_LOCAL_PORT)
                .map(|key_val| &key_val.value),
            Some(&8080.into())
        );
    }

    #[test]
    fn test_http_server_network_peer_address() {
        let server = HttpServerAttributes {
            network_peer_address: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder()
            .uri(Uri::from_static("https://localhost/graphql"))
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == NETWORK_PEER_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"192.168.0.8".into())
        );
    }

    #[test]
    fn test_http_server_network_peer_port() {
        let server = HttpServerAttributes {
            network_peer_port: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder()
            .uri(Uri::from_static("https://localhost/graphql"))
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == NETWORK_PEER_PORT)
                .map(|key_val| &key_val.value),
            Some(&6060.into())
        );
    }

    #[test]
    fn test_http_server_server_address() {
        let server = HttpServerAttributes {
            server_address: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder().build().unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SERVER_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"192.168.0.1".into())
        );

        let mut req = router::Request::fake_builder()
            .header(FORWARDED, "host=2.4.6.8:8000")
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SERVER_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"2.4.6.8".into())
        );
    }

    #[test]
    fn test_http_server_server_port() {
        let server = HttpServerAttributes {
            server_port: Some(true),
            ..Default::default()
        };

        let mut req = router::Request::fake_builder().build().unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SERVER_PORT)
                .map(|key_val| &key_val.value),
            Some(&8080.into())
        );

        let mut req = router::Request::fake_builder()
            .header(FORWARDED, "host=2.4.6.8:8000")
            .build()
            .unwrap();
        req.router_request.extensions_mut().insert(ConnectionInfo {
            peer_address: Some(SocketAddr::from_str("192.168.0.8:6060").unwrap()),
            server_address: Some(SocketAddr::from_str("192.168.0.1:8080").unwrap()),
        });
        let attributes = server.on_request(&req);
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SERVER_PORT)
                .map(|key_val| &key_val.value),
            Some(&8000.into())
        );
    }
    #[test]
    fn test_http_server_url_path() {
        let server = HttpServerAttributes {
            url_path: Some(true),
            ..Default::default()
        };

        let attributes = server.on_request(
            &router::Request::fake_builder()
                .uri(Uri::from_static("https://localhost/graphql"))
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == URL_PATH)
                .map(|key_val| &key_val.value),
            Some(&"/graphql".into())
        );
    }
    #[test]
    fn test_http_server_query() {
        let server = HttpServerAttributes {
            url_query: Some(true),
            ..Default::default()
        };

        let attributes = server.on_request(
            &router::Request::fake_builder()
                .uri(Uri::from_static("https://localhost/graphql?hi=5"))
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == URL_QUERY)
                .map(|key_val| &key_val.value),
            Some(&"hi=5".into())
        );
    }
    #[test]
    fn test_http_server_scheme() {
        let server = HttpServerAttributes {
            url_scheme: Some(true),
            ..Default::default()
        };

        let attributes = server.on_request(
            &router::Request::fake_builder()
                .uri(Uri::from_static("https://localhost/graphql"))
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == URL_SCHEME)
                .map(|key_val| &key_val.value),
            Some(&"https".into())
        );
    }

    #[test]
    fn test_http_server_user_agent_original() {
        let server = HttpServerAttributes {
            user_agent_original: Some(true),
            ..Default::default()
        };

        let attributes = server.on_request(
            &router::Request::fake_builder()
                .header(USER_AGENT, HeaderValue::from_static("my-agent"))
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == USER_AGENT_ORIGINAL)
                .map(|key_val| &key_val.value),
            Some(&"my-agent".into())
        );
    }
}
