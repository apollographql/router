use std::collections::HashMap;

use http::header::CONTENT_LENGTH;
use http::header::USER_AGENT;
use opentelemetry_api::Key;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;
use tower::BoxError;

use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::config::AttributeValue;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

/// This struct can be used as an attributes container, it has a custom JsonSchema implementation that will merge the schemas of the attributes and custom fields.
#[allow(dead_code)]
#[derive(Clone, Deserialize, Debug, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Extendable<A, E>
where
    A: Default,
{
    attributes: A,

    custom: HashMap<String, E>,
}

impl Extendable<(), ()> {
    pub(crate) fn empty<A, E>() -> Extendable<A, E>
    where
        A: Default,
    {
        Default::default()
    }
}

impl<A, E> Default for Extendable<A, E>
where
    A: Default,
{
    fn default() -> Self {
        Self {
            attributes: Default::default(),
            custom: HashMap::new(),
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum RouterEvent {
    /// When a service request occurs.
    Request,
    /// When a service response occurs.
    Response,
    /// When a service error occurs.
    Error,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum DefaultAttributeRequirementLevel {
    /// Attributes that are marked as required in otel semantic conventions and apollo documentation will be included (default)
    #[default]
    Required,
    /// Attributes that are marked as required or recommended in otel semantic conventions and apollo documentation will be included
    Recommended,
    /// Attributes that are marked as required, recommended or opt-in in otel semantic conventions and apollo documentation will be included
    OptIn,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum TraceIdFormat {
    /// Open Telemetry trace ID, a hex string.
    OpenTelemetry,
    /// Datadog trace ID, a u64.
    Datadog,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum RouterCustomAttribute {
    /// A header from the request
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A header from the response
    ResponseHeader {
        /// The name of the request header.
        response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// The trace ID of the request.
    TraceId {
        /// The format of the trace ID.
        trace_id: TraceIdFormat,
    },
    /// A value from context.
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from baggage.
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from an environment variable.
    Env {
        /// The name of the environment variable
        env: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum SupergraphCustomAttribute {
    OperationName {
        /// The operation name from the query.
        operation_name: OperationName,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    OperationKind {
        /// The operation kind from the query (query|mutation|subscription).
        operation_kind: OperationKind,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    QueryVariable {
        /// The name of a graphql query variable.
        query_variable: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseHeader {
        /// The name of the response header.
        response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    RequestContext {
        /// The request context key.
        request_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    Env {
        /// The name of the environment variable
        env: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case", untagged)]
pub(crate) enum SubgraphCustomAttribute {
    SubgraphOperationName {
        /// The operation name from the subgraph query.
        subgraph_operation_name: OperationName,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphOperationKind {
        /// The kind of the subgraph operation (query|mutation|subscription).
        subgraph_operation_kind: OperationKind,
    },
    SubgraphQueryVariable {
        /// The name of a subgraph query variable.
        subgraph_query_variable: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphResponseBody {
        /// The subgraph response body json path.
        subgraph_response_body: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphRequestHeader {
        /// The name of the subgraph request header.
        subgraph_request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphResponseHeader {
        /// The name of the subgraph response header.
        subgraph_response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },

    SupergraphOperationName {
        /// The supergraph query operation name.
        supergraph_operation_name: OperationName,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphOperationKind {
        /// The supergraph query operation kind (query|mutation|subscription).
        supergraph_operation_kind: OperationKind,
    },
    SupergraphQueryVariable {
        /// The supergraph query variable name.
        supergraph_query_variable: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphResponseBody {
        /// The supergraph response body json path.
        supergraph_response_body: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphRequestHeader {
        /// The supergraph request header name.
        supergraph_request_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphResponseHeader {
        /// The supergraph response header name.
        supergraph_response_header: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    RequestContext {
        /// The request context key.
        request_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseContext {
        /// The response context key.
        response_context: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    Env {
        /// The name of the environment variable
        env: String,
        #[serde(skip)]
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(default)]
pub(crate) struct RouterAttributes {
    /// Http attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    common: HttpCommonAttributes,
    /// Http server attributes from Open Telemetry semantic conventions.
    // TODO: unskip it and add it gradually
    #[serde(flatten, skip)]
    server: HttpServerAttributes,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(default)]
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

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(default)]
pub(crate) struct SubgraphAttributes {
    /// The name of the subgraph
    /// Examples:
    /// * products
    /// Requirement level: Required
    #[serde(rename = "graphql.federation.subgraph.name")]
    graphql_federation_subgraph_name: Option<bool>,
    /// The GraphQL document being executed.
    /// Examples:
    /// * query findBookById { bookById(id: ?) { name } }
    /// Requirement level: Recommended
    #[serde(rename = "graphql.document")]
    graphql_document: Option<bool>,
    /// The name of the operation being executed.
    /// Examples:
    /// * findBookById
    /// Requirement level: Recommended
    #[serde(rename = "graphql.operation.name")]
    graphql_operation_name: Option<bool>,
    /// The type of the operation being executed.
    /// Examples:
    /// * query
    /// * subscription
    /// * mutation
    /// Requirement level: Recommended
    #[serde(rename = "graphql.operation.type")]
    graphql_operation_type: Option<bool>,
}

/// Common attributes for http server and client.
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#common-attributes
#[allow(dead_code)]
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
    /// Requirement level: Conditionally Required
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
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpServerAttributes {
    /// Client address - domain name if available without reverse DNS lookup, otherwise IP address or Unix domain socket name.
    /// Examples:
    /// * 83.164.160.102
    /// Requirement level: Recommended
    #[serde(rename = "client.address")]
    client_address: Option<bool>,
    /// The port of the original client behind all proxies, if known (e.g. from Forwarded or a similar header). Otherwise, the immediate client peer port.
    /// Examples:
    /// * 83.164.160.102
    /// Requirement level: Recommended
    #[serde(rename = "client.port")]
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
    #[serde(rename = "network.local.address")]
    network_local_address: Option<bool>,
    /// Local socket port. Useful in case of a multi-port host.
    /// Examples:
    /// * 65123
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.port")]
    network_local_port: Option<bool>,
    /// Peer address of the network connection - IP address or Unix domain socket name.
    /// Examples:
    /// * 10.1.2.80
    /// * /tmp/my.sock
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.address")]
    network_peer_address: Option<bool>,
    /// Peer port number of the network connection.
    /// Examples:
    /// * 65123
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.port")]
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

/// Attrubtes for HTTP clients
/// https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-client
#[allow(dead_code)]
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

pub(crate) trait GetAttributes<Request, Response> {
    fn on_request(&self, request: &Request) -> HashMap<Key, AttributeValue>;
    fn on_response(&self, response: &Response) -> HashMap<Key, AttributeValue>;
    fn on_error(&self, error: &BoxError) -> HashMap<Key, AttributeValue>;
}

pub(crate) trait GetAttribute<Request, Response> {
    fn on_request(&self, request: &Request) -> Option<AttributeValue>;
    fn on_response(&self, response: &Response) -> Option<AttributeValue>;
}

impl<A, E, Request, Response> GetAttributes<Request, Response> for Extendable<A, E>
where
    A: Default + GetAttributes<Request, Response>,
    E: GetAttribute<Request, Response>,
{
    fn on_request(&self, request: &Request) -> HashMap<Key, AttributeValue> {
        let mut attrs = self.attributes.on_request(request);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_request(request)
                .map(|v| (Key::from(key.clone()), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_response(&self, response: &Response) -> HashMap<Key, AttributeValue> {
        let mut attrs = self.attributes.on_response(response);
        let custom_attributes = self.custom.iter().filter_map(|(key, value)| {
            value
                .on_response(response)
                .map(|v| (Key::from(key.clone()), v))
        });
        attrs.extend(custom_attributes);

        attrs
    }

    fn on_error(&self, error: &BoxError) -> HashMap<Key, AttributeValue> {
        self.attributes.on_error(error)
    }
}

impl GetAttribute<router::Request, router::Response> for RouterCustomAttribute {
    fn on_request(&self, request: &router::Request) -> Option<AttributeValue> {
        match self {
            RouterCustomAttribute::RequestHeader {
                request_header,
                default,
                ..
            } => request
                .router_request
                .headers()
                .get(request_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            RouterCustomAttribute::Env { env, default, .. } => std::env::var(env)
                .ok()
                .map(AttributeValue::String)
                .or_else(|| default.clone().map(AttributeValue::String)),
            RouterCustomAttribute::TraceId { trace_id } => todo!(),
            RouterCustomAttribute::Baggage {
                baggage,
                redact,
                default,
            } => todo!(),
            // Related to Response
            _ => None,
        }
    }

    fn on_response(&self, response: &router::Response) -> Option<AttributeValue> {
        match self {
            RouterCustomAttribute::ResponseHeader {
                response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(response_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            RouterCustomAttribute::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get(response_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            RouterCustomAttribute::TraceId { trace_id } => todo!(),
            RouterCustomAttribute::Baggage {
                baggage,
                redact,
                default,
            } => todo!(),
            _ => None,
        }
    }
}

impl GetAttributes<router::Request, router::Response> for RouterAttributes {
    fn on_request(&self, request: &router::Request) -> HashMap<Key, AttributeValue> {
        let mut attrs = self.common.on_request(request);

        attrs
    }

    fn on_response(&self, response: &router::Response) -> HashMap<Key, AttributeValue> {
        let mut attrs = self.common.on_response(response);
        attrs
    }

    fn on_error(&self, error: &BoxError) -> HashMap<Key, AttributeValue> {
        let mut attrs = self.common.on_error(error);
        attrs
    }
}

impl GetAttributes<router::Request, router::Response> for HttpCommonAttributes {
    fn on_request(&self, request: &router::Request) -> HashMap<Key, AttributeValue> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.http_request_body_size {
            if let Some(content_length) = request
                .router_request
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            {
                attrs.insert(
                    "http.request.body.size".into(),
                    AttributeValue::String(content_length.to_string()),
                );
            }
        }
        if let Some(true) = &self.network_protocol_name {
            attrs.insert(
                "network.protocol.name".into(),
                AttributeValue::String("http".to_string()),
            );
        }
        if let Some(true) = &self.network_protocol_version {
            attrs.insert(
                "network.protocol.version".into(),
                AttributeValue::String(format!("{:?}", request.router_request.version())),
            );
        }
        if let Some(true) = &self.network_transport {
            attrs.insert(
                "network.protocol.transport".into(),
                AttributeValue::String("tcp".to_string()),
            );
        }
        if let Some(true) = &self.user_agent_original {
            if let Some(user_agent) = request
                .router_request
                .headers()
                .get(&USER_AGENT)
                .and_then(|h| h.to_str().ok())
            {
                attrs.insert(
                    "user_agent.original".into(),
                    AttributeValue::String(user_agent.to_string()),
                );
            }
        }

        attrs
    }

    fn on_response(&self, response: &router::Response) -> HashMap<Key, AttributeValue> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.http_response_body_size {
            if let Some(content_length) = response
                .response
                .headers()
                .get(&CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
            {
                attrs.insert(
                    "http.response.body.size".into(),
                    AttributeValue::String(content_length.to_string()),
                );
            }
        }
        if let Some(true) = &self.http_response_status_code {
            attrs.insert(
                "http.response.status_code".into(),
                AttributeValue::String(response.response.status().to_string()),
            );
        }
        attrs
    }

    fn on_error(&self, error: &BoxError) -> HashMap<Key, AttributeValue> {
        let mut attrs = HashMap::new();
        if let Some(true) = &self.error_type {
            attrs.insert("error.type".into(), AttributeValue::I64(500));
        }

        attrs
    }
}

impl GetAttribute<supergraph::Request, supergraph::Response> for SupergraphCustomAttribute {
    fn on_request(&self, request: &supergraph::Request) -> Option<AttributeValue> {
        match self {
            SupergraphCustomAttribute::OperationName {
                operation_name,
                default,
                ..
            } => {
                let op_name = request.context.get(OPERATION_NAME).ok().flatten();
                match operation_name {
                    OperationName::String => {
                        op_name.or_else(|| default.clone().map(AttributeValue::String))
                    }
                    OperationName::Hash => todo!(),
                }
            }
            SupergraphCustomAttribute::OperationKind { default, .. } => request
                .context
                .get(OPERATION_KIND)
                .ok()
                .flatten()
                .or_else(|| default.clone().map(AttributeValue::String)),
            SupergraphCustomAttribute::QueryVariable {
                query_variable,
                default,
                ..
            } => request
                .supergraph_request
                .body()
                .variables
                .get(&ByteString::from(query_variable.as_str()))
                .and_then(|v| serde_json::to_string(v).ok())
                .map(AttributeValue::String)
                .or_else(|| default.clone()),
            SupergraphCustomAttribute::RequestHeader {
                request_header,
                default,
                ..
            } => request
                .supergraph_request
                .headers()
                .get(request_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SupergraphCustomAttribute::RequestContext {
                request_context,
                default,
                ..
            } => request
                .context
                .get(request_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            SupergraphCustomAttribute::Baggage {
                baggage, default, ..
            } => todo!(),
            SupergraphCustomAttribute::Env { env, default, .. } => std::env::var(env)
                .ok()
                .map(AttributeValue::String)
                .or_else(|| default.clone().map(AttributeValue::String)),
            // For response
            _ => None,
        }
    }

    fn on_response(&self, response: &supergraph::Response) -> Option<AttributeValue> {
        match self {
            SupergraphCustomAttribute::ResponseHeader {
                response_header,
                default,
                ..
            } => response
                .response
                .headers()
                .get(response_header)
                .and_then(|h| Some(AttributeValue::String(h.to_str().ok()?.to_string())))
                .or_else(|| default.clone()),
            SupergraphCustomAttribute::ResponseContext {
                response_context,
                default,
                ..
            } => response
                .context
                .get(response_context)
                .ok()
                .flatten()
                .or_else(|| default.clone()),
            // For request
            _ => None,
        }
    }
}
