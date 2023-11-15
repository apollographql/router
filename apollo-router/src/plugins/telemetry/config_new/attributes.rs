use std::collections::HashMap;
use std::fmt::Debug;

use http::header::CONTENT_LENGTH;
use http::header::USER_AGENT;
use opentelemetry_api::Key;
use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_BODY_SIZE;
use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_BODY_SIZE;
use opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_STATUS_CODE;
use opentelemetry_semantic_conventions::trace::HTTP_ROUTE;
use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_NAME;
use opentelemetry_semantic_conventions::trace::NETWORK_PROTOCOL_VERSION;
use opentelemetry_semantic_conventions::trace::NETWORK_TRANSPORT;
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

use crate::context::OPERATION_KIND;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::query_planner::OperationKind;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
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

    /// Http attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    pub(crate) common: HttpCommonAttributes,
    /// Http server attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    pub(crate) server: HttpServerAttributes,
}

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(Serialize))]
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

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphAttributes {
    /// The name of the subgraph
    /// Examples:
    /// * products
    /// Requirement level: Required
    #[serde(rename = "subgraph.name")]
    pub(crate) graphql_federation_subgraph_name: Option<bool>,
    /// The GraphQL document being executed.
    /// Examples:
    /// * query findBookById { bookById(id: ?) { name } }
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.document")]
    pub(crate) graphql_document: Option<bool>,
    /// The name of the operation being executed.
    /// Examples:
    /// * findBookById
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.name")]
    pub(crate) graphql_operation_name: Option<bool>,
    /// The type of the operation being executed.
    /// Examples:
    /// * query
    /// * subscription
    /// * mutation
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.type")]
    pub(crate) graphql_operation_type: Option<bool>,
}

/// Common attributes for http server and client.
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#common-attributes
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
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
    #[serde(rename = "http.request.method.original")]
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

    /// Value of the HTTP User-Agent header sent by the client.
    /// Examples:
    /// * CERN-LineMode/2.15
    /// * libwww/2.17b3
    /// Requirement level: Recommended
    #[serde(rename = "user_agent.original")]
    pub(crate) user_agent_original: Option<bool>,
}

/// Attributes for Http servers
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-server
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpServerAttributes {
    /// Client address - domain name if available without reverse DNS lookup, otherwise IP address or Unix domain socket name.
    /// Examples:
    /// * 83.164.160.102
    /// Requirement level: Recommended
    #[serde(rename = "client.address", skip)]
    client_address: Option<bool>,
    /// The port of the original client behind all proxies, if known (e.g. from Forwarded or a similar header). Otherwise, the immediate client peer port.
    /// Examples:
    /// * 83.164.160.102
    /// Requirement level: Recommended
    #[serde(rename = "client.port", skip)]
    client_port: Option<bool>,
    /// The matched route (path template in the format used by the respective server framework).
    /// Examples:
    /// * 65123
    /// Requirement level: Conditionally Required: If and only if it’s available
    #[serde(rename = "http.route")]
    http_route: Option<bool>,
    /// Local socket address. Useful in case of a multi-IP host.
    /// Examples:
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.address", skip)]
    network_local_address: Option<bool>,
    /// Local socket port. Useful in case of a multi-port host.
    /// Examples:
    /// * 65123
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.port", skip)]
    network_local_port: Option<bool>,
    /// Peer address of the network connection - IP address or Unix domain socket name.
    /// Examples:
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.address", skip)]
    network_peer_address: Option<bool>,
    /// Peer port number of the network connection.
    /// Examples:
    /// * 65123
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.port", skip)]
    network_peer_port: Option<bool>,
    /// Name of the local HTTP server that received the request.
    /// Examples:
    /// * example.com
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Recommended
    #[serde(rename = "server.address")]
    server_address: Option<bool>,
    /// Port of the local HTTP server that received the request.
    /// Examples:
    /// * 80
    /// * 8080
    /// * 443
    /// Requirement level: Recommended
    #[serde(rename = "server.port")]
    server_port: Option<bool>,
    /// The URI path component
    /// Examples:
    /// * /search
    /// Requirement level: Required
    #[serde(rename = "url.path")]
    url_path: Option<bool>,
    /// The URI query component
    /// Examples:
    /// * q=OpenTelemetry
    /// Requirement level: Conditionally Required: If and only if one was received/sent.
    #[serde(rename = "url.query")]
    url_query: Option<bool>,

    /// The URI scheme component identifying the used protocol.
    /// Examples:
    /// * http
    /// * https
    /// Requirement level: Required
    #[serde(rename = "url.scheme")]
    url_scheme: Option<bool>,
}

/// Attributes for HTTP clients
/// https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-client
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpClientAttributes {
    /// The ordinal number of request resending attempt.
    /// Examples:
    /// *
    /// Requirement level: Recommended: if and only if request was retried.
    #[serde(rename = "http.resend_count")]
    http_resend_count: Option<bool>,

    /// Peer address of the network connection - IP address or Unix domain socket name.
    /// Examples:
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Recommended: If different than server.address.
    #[serde(rename = "network.peer.address")]
    network_peer_address: Option<bool>,

    /// Peer port number of the network connection.
    /// Examples:
    /// * 65123
    /// Requirement level: Recommended: If network.peer.address is set.
    #[serde(rename = "network.peer.port")]
    network_peer_port: Option<bool>,

    /// Host identifier of the “URI origin” HTTP request is sent to.
    /// Examples:
    /// * example.com
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Required
    #[serde(rename = "server.address")]
    server_address: Option<bool>,

    /// Port identifier of the “URI origin” HTTP request is sent to.
    /// Examples:
    /// * 80
    /// * 8080
    /// * 433
    /// Requirement level: Conditionally Required
    #[serde(rename = "server.port")]
    server_port: Option<bool>,

    /// Absolute URL describing a network resource according to RFC3986
    /// Examples:
    /// * https://www.foo.bar/search?q=OpenTelemetry#SemConv;
    /// * localhost
    /// Requirement level: Required
    #[serde(rename = "url.full")]
    url_full: Option<bool>,
}

impl Selectors for RouterAttributes {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = self.common.on_request(request);
        attrs.extend(self.server.on_request(request));
        if let Some(true) = &self.trace_id {
            if let Some(trace_id) = trace_id() {
                attrs.insert(
                    Key::from_static_str("trace_id"),
                    trace_id.to_string().into(),
                );
            }
        }
        if let Some(true) = &self.datadog_trace_id {
            if let Some(trace_id) = trace_id() {
                attrs.insert(
                    Key::from_static_str("dd.trace_id"),
                    trace_id.to_datadog().into(),
                );
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = self.common.on_response(response);
        attrs.extend(self.server.on_response(response));
        attrs
    }

    fn on_error(&self, error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = self.common.on_error(error);
        attrs.extend(self.server.on_error(error));
        attrs
    }
}

impl Selectors for HttpCommonAttributes {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.http_request_body_size {
            if let Some(content_length) = request
                .router_request
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            {
                attrs.insert(HTTP_REQUEST_BODY_SIZE, content_length.to_string().into());
            }
        }
        if let Some(true) = &self.network_protocol_name {
            attrs.insert(NETWORK_PROTOCOL_NAME, "http".into());
        }
        if let Some(true) = &self.network_protocol_version {
            attrs.insert(
                NETWORK_PROTOCOL_VERSION,
                format!("{:?}", request.router_request.version()).into(),
            );
        }
        if let Some(true) = &self.network_transport {
            attrs.insert(NETWORK_TRANSPORT, "tcp".to_string().into());
        }
        if let Some(true) = &self.user_agent_original {
            if let Some(user_agent) = request
                .router_request
                .headers()
                .get(&USER_AGENT)
                .and_then(|h| h.to_str().ok())
            {
                attrs.insert(USER_AGENT_ORIGINAL, user_agent.to_string().into());
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.http_response_body_size {
            if let Some(content_length) = response
                .response
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            {
                attrs.insert(HTTP_RESPONSE_BODY_SIZE, content_length.to_string().into());
            }
        }
        if let Some(true) = &self.http_response_status_code {
            attrs.insert(
                HTTP_RESPONSE_STATUS_CODE,
                (response.response.status().as_u16() as i64).into(),
            );
        }
        attrs
    }

    fn on_error(&self, _error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.error_type {
            attrs.insert(Key::from_static_str("error.type"), 500.into());
        }

        attrs
    }
}

impl Selectors for HttpServerAttributes {
    type Request = router::Request;
    type Response = router::Response;

    fn on_request(&self, request: &router::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.http_route {
            attrs.insert(HTTP_ROUTE, request.router_request.uri().to_string().into());
        }
        let router_uri = request.router_request.uri();
        if let Some(true) = &self.server_address {
            if let Some(host) = router_uri.host() {
                attrs.insert(SERVER_ADDRESS, host.to_string().into());
            }
        }
        if let Some(true) = &self.server_port {
            if let Some(port) = router_uri.port_u16() {
                attrs.insert(SERVER_PORT, (port as i64).into());
            }
        }
        if let Some(true) = &self.url_path {
            attrs.insert(URL_PATH, router_uri.path().to_string().into());
        }
        if let Some(true) = &self.url_query {
            if let Some(query) = router_uri.query() {
                attrs.insert(URL_QUERY, query.to_string().into());
            }
        }
        if let Some(true) = &self.url_scheme {
            if let Some(scheme) = router_uri.scheme_str() {
                attrs.insert(URL_SCHEME, scheme.to_string().into());
            }
        }

        attrs
    }

    fn on_response(&self, _response: &router::Response) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }

    fn on_error(&self, _error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }
}

impl DefaultForLevel for HttpCommonAttributes {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel) {
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
                // Required
                if self.error_type.is_none() {
                    self.error_type = Some(true);
                }

                if self.http_request_method.is_none() {
                    self.http_request_method = Some(true);
                }

                if self.error_type.is_none() {
                    self.error_type = Some(true);
                }
                if self.http_response_status_code.is_none() {
                    self.http_response_status_code = Some(true);
                }

                // Recommended
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
                if self.user_agent_original.is_none() {
                    self.user_agent_original = Some(true);
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors for SupergraphAttributes {
    type Request = supergraph::Request;
    type Response = supergraph::Response;

    fn on_request(&self, request: &supergraph::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.graphql_document {
            if let Some(query) = &request.supergraph_request.body().query {
                attrs.insert(GRAPHQL_DOCUMENT, query.clone().into());
            }
        }
        if let Some(true) = &self.graphql_operation_name {
            if let Some(op_name) = &request.supergraph_request.body().operation_name {
                attrs.insert(GRAPHQL_OPERATION_NAME, op_name.clone().into());
            }
        }
        if let Some(true) = &self.graphql_operation_type {
            let operation_kind: OperationKind = request
                .context
                .get(OPERATION_KIND)
                .ok()
                .flatten()
                .unwrap_or_default();
            attrs.insert(
                GRAPHQL_OPERATION_TYPE,
                operation_kind.as_apollo_operation_type().clone().into(),
            );
        }

        attrs
    }

    fn on_response(&self, _response: &supergraph::Response) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }

    fn on_error(&self, _error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }
}

impl Selectors for SubgraphAttributes {
    type Request = subgraph::Request;
    type Response = subgraph::Response;

    fn on_request(&self, request: &subgraph::Request) -> HashMap<Key, opentelemetry::Value> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.graphql_document {
            if let Some(query) = &request.supergraph_request.body().query {
                attrs.insert(
                    Key::from_static_str("subgraph.graphql.document"),
                    query.clone().into(),
                );
            }
        }
        if let Some(true) = &self.graphql_operation_name {
            if let Some(op_name) = &request.supergraph_request.body().operation_name {
                attrs.insert(
                    Key::from_static_str("subgraph.graphql.operation.name"),
                    op_name.clone().into(),
                );
            }
        }
        if let Some(true) = &self.graphql_operation_type {
            let operation_kind: OperationKind = request
                .context
                .get(OPERATION_KIND)
                .ok()
                .flatten()
                .unwrap_or_default();
            attrs.insert(
                Key::from_static_str("subgraph.graphql.operation.type"),
                operation_kind.as_apollo_operation_type().into(),
            );
        }
        if let Some(true) = &self.graphql_federation_subgraph_name {
            if let Some(subgraph_name) = &request.subgraph_name {
                attrs.insert(
                    Key::from_static_str("subgraph.name"),
                    subgraph_name.clone().into(),
                );
            }
        }

        attrs
    }

    fn on_response(&self, _response: &subgraph::Response) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }

    fn on_error(&self, _error: &BoxError) -> HashMap<Key, opentelemetry::Value> {
        HashMap::with_capacity(0)
    }
}
