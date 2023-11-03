use std::any::type_name;
use std::collections::HashMap;
use std::fmt::Debug;

use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::JsonSchema;
use serde::de::Error;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Deserializer;
#[cfg(test)]
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::plugins::telemetry::config::AttributeValue;

/// This struct can be used as an attributes container, it has a custom JsonSchema implementation that will merge the schemas of the attributes and custom fields.
#[allow(dead_code)]
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
pub(crate) struct Extendable<Att, Ext>
where
    Att: Default,
{
    attributes: Att,
    custom: HashMap<String, Ext>,
}

impl Extendable<(), ()> {
    pub(crate) fn empty<A, E>() -> Extendable<A, E>
    where
        A: Default,
    {
        Default::default()
    }
}

/// Custom Deserializer for attributes that will deserializse into a custom field if possible, but otherwise into one of the pre-defined attributes.
impl<'de, Att, Ext> Deserialize<'de> for Extendable<Att, Ext>
where
    Att: Default + Deserialize<'de> + Debug + Sized,
    Ext: Deserialize<'de> + Debug + Sized,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ExtendableVisitor<Att, Ext> {
            _phantom: std::marker::PhantomData<(Att, Ext)>,
        }
        impl<'de, Att, Ext> Visitor<'de> for ExtendableVisitor<Att, Ext>
        where
            Att: Default + Deserialize<'de> + Debug,
            Ext: Deserialize<'de> + Debug,
        {
            type Value = Extendable<Att, Ext>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a map structure")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut attributes: Map<String, Value> = Map::new();
                let mut custom: HashMap<String, Ext> = HashMap::new();
                while let Some(key) = map.next_key()? {
                    let value: Value = map.next_value()?;
                    match Ext::deserialize(value.clone()) {
                        Ok(value) => {
                            custom.insert(key, value);
                        }
                        Err(_err) => {
                            // We didn't manage to deserialize as a custom attribute, so stash the value and we'll try again later
                            attributes.insert(key, value);
                        }
                    }
                }

                let attributes =
                    Att::deserialize(Value::Object(attributes)).map_err(A::Error::custom)?;

                Ok(Extendable { attributes, custom })
            }
        }

        deserializer.deserialize_map(ExtendableVisitor::<Att, Ext> {
            _phantom: Default::default(),
        })
    }
}

impl<A, E> JsonSchema for Extendable<A, E>
where
    A: Default + JsonSchema,
    E: JsonSchema,
{
    fn schema_name() -> String {
        format!(
            "extendable_attribute_{}_{}",
            type_name::<A>(),
            type_name::<E>()
        )
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        let mut attributes = gen.subschema_for::<A>();
        let custom = gen.subschema_for::<HashMap<String, E>>();
        if let Schema::Object(schema) = &mut attributes {
            if let Some(object) = &mut schema.object {
                object.additional_properties =
                    custom.into_object().object().additional_properties.clone();
            }
        }

        attributes
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
    /// Attributes that are marked as required or recommended in otel semantic conventions and apollo documentation will be included
    Recommended,

    /// Attributes that are marked as required in otel semantic conventions and apollo documentation will be included (default)
    #[default]
    Required,
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
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    /// A header from the response
    ResponseHeader {
        /// The name of the request header.
        response_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
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
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    /// A value from baggage.
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    /// A value from an environment variable.
    Env {
        /// The name of the environment variable
        env: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}
#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationName {
    /// The raw operation name.
    String,
    /// A hash of the operation name.
    Hash,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Query {
    /// The raw query kind.
    String,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum OperationKind {
    /// The raw operation kind.
    String,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum SupergraphCustomAttribute {
    OperationName {
        /// The operation name from the query.
        operation_name: OperationName,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    OperationKind {
        /// The operation kind from the query (query|mutation|subscription).
        operation_kind: OperationKind,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    Query {
        /// The graphql query.
        query: Query,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    QueryVariable {
        /// The name of a graphql query variable.
        query_variable: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    ResponseBody {
        /// Json Path into the response body
        response_body: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    RequestHeader {
        /// The name of the request header.
        request_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    ResponseHeader {
        /// The name of the response header.
        response_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    RequestContext {
        /// The request context key.
        request_context: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseContext {
        /// The response context key.
        response_context: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    Env {
        /// The name of the environment variable
        env: String,
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
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphOperationKind {
        /// The kind of the subgraph operation (query|mutation|subscription).
        subgraph_operation_kind: OperationKind,
    },
    SubgraphQuery {
        /// The graphql query to the subgraph.
        subgraph_query: Query,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphQueryVariable {
        /// The name of a subgraph query variable.
        subgraph_query_variable: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphResponseBody {
        /// The subgraph response body json path.
        subgraph_response_body: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphRequestHeader {
        /// The name of the subgraph request header.
        subgraph_request_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SubgraphResponseHeader {
        /// The name of the subgraph response header.
        subgraph_response_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },

    SupergraphOperationName {
        /// The supergraph query operation name.
        supergraph_operation_name: OperationName,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphOperationKind {
        /// The supergraph query operation kind (query|mutation|subscription).
        supergraph_operation_kind: OperationKind,
    },
    SupergraphQuery {
        /// The supergraph query to the subgraph.
        supergraph_query: Query,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphQueryVariable {
        /// The supergraph query variable name.
        supergraph_query_variable: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphResponseBody {
        /// The supergraph response body json path.
        supergraph_response_body: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphRequestHeader {
        /// The supergraph request header name.
        supergraph_request_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    SupergraphResponseHeader {
        /// The supergraph response header name.
        supergraph_response_header: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    RequestContext {
        /// The request context key.
        request_context: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    ResponseContext {
        /// The response context key.
        response_context: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<AttributeValue>,
    },
    Baggage {
        /// The name of the baggage item.
        baggage: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
    Env {
        /// The name of the environment variable
        env: String,
        /// Optional redaction pattern.
        redact: Option<String>,
        /// Optional default value.
        default: Option<String>,
    },
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RouterAttributes {
    /// Http attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    common: HttpCommonAttributes,
    /// Http server attributes from Open Telemetry semantic conventions.
    #[serde(flatten)]
    server: HttpServerAttributes,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphAttributes {
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

#[allow(dead_code)]
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
    subgraph_graphql_document: Option<bool>,
    /// The name of the operation being executed.
    /// Examples:
    /// * findBookById
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.name")]
    subgraph_graphql_operation_name: Option<bool>,
    /// The type of the operation being executed.
    /// Examples:
    /// * query
    /// * subscription
    /// * mutation
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.type")]
    subgraph_graphql_operation_type: Option<bool>,
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
    error_type: Option<bool>,

    /// The size of the request payload body in bytes. This is the number of bytes transferred excluding headers and is often, but not always, present as the Content-Length header. For requests using transport encoding, this should be the compressed size.
    /// Examples:
    /// * 3495
    /// Requirement level: Recommended
    #[serde(rename = "http.request.body.size")]
    http_request_body_size: Option<bool>,

    /// HTTP request method.
    /// Examples:
    /// * GET
    /// * POST
    /// * HEAD
    /// Requirement level: Required
    #[serde(rename = "http.request.method")]
    http_request_method: Option<bool>,

    /// Original HTTP method sent by the client in the request line.
    /// Examples:
    /// * GeT
    /// * ACL
    /// * foo
    /// Requirement level: Conditionally Required
    #[serde(rename = "http.request.method.original")]
    http_request_method_original: Option<bool>,

    /// The size of the response payload body in bytes. This is the number of bytes transferred excluding headers and is often, but not always, present as the Content-Length header. For requests using transport encoding, this should be the compressed size.
    /// Examples:
    /// * 3495
    /// Requirement level: Recommended
    #[serde(rename = "http.response.body.size")]
    http_response_body_size: Option<bool>,

    /// HTTP response status code.
    /// Examples:
    /// * 200
    /// Requirement level: Conditionally Required: If and only if one was received/sent.
    #[serde(rename = "http.response.status_code")]
    http_response_status_code: Option<bool>,

    /// OSI application layer or non-OSI equivalent.
    /// Examples:
    /// * http
    /// * spdy
    /// Requirement level: Recommended: if not default (http).
    #[serde(rename = "network.protocol.name")]
    network_protocol_name: Option<bool>,

    /// Version of the protocol specified in network.protocol.name.
    /// Examples:
    /// * 1.0
    /// * 1.1
    /// * 2
    /// * 3
    /// Requirement level: Recommended
    #[serde(rename = "network.protocol.version")]
    network_protocol_version: Option<bool>,

    /// OSI transport layer.
    /// Examples:
    /// * tcp
    /// * udp
    /// Requirement level: Conditionally Required
    #[serde(rename = "network.transport")]
    network_transport: Option<bool>,

    /// OSI network layer or non-OSI equivalent.
    /// Examples:
    /// * ipv4
    /// * ipv6
    /// Requirement level: Recommended
    #[serde(rename = "network.type")]
    network_type: Option<bool>,

    /// Value of the HTTP User-Agent header sent by the client.
    /// Examples:
    /// * CERN-LineMode/2.15
    /// * libwww/2.17b3
    /// Requirement level: Recommended
    #[serde(rename = "user_agent.original")]
    user_agent_original: Option<bool>,
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

/// Attributes for HTTP clients
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

#[cfg(test)]
mod test {
    use insta::assert_yaml_snapshot;

    use crate::plugins::telemetry::config_new::attributes::Extendable;
    use crate::plugins::telemetry::config_new::attributes::SupergraphAttributes;
    use crate::plugins::telemetry::config_new::attributes::SupergraphCustomAttribute;

    #[test]
    fn test_extendable_serde() {
        let mut settings = insta::Settings::clone_current();
        settings.set_sort_maps(true);
        settings.bind(|| {
            let o = serde_json::from_value::<
                Extendable<SupergraphAttributes, SupergraphCustomAttribute>,
            >(serde_json::json!({
                    "graphql.operation.name": true,
                    "graphql.operation.type": true,
                    "custom_1": {
                        "operation_name": "string"
                    },
                    "custom_2": {
                        "operation_name": "string"
                    }
            }))
            .unwrap();
            assert_yaml_snapshot!(o);
        });
    }

    #[test]
    fn test_extendable_serde_fail() {
        serde_json::from_value::<Extendable<SupergraphAttributes, SupergraphCustomAttribute>>(
            serde_json::json!({
                    "graphql.operation": true,
                    "graphql.operation.type": true,
                    "custom_1": {
                        "operation_name": "string"
                    },
                    "custom_2": {
                        "operation_name": "string"
                    }
            }),
        )
        .expect_err("Should have errored");
    }
}
