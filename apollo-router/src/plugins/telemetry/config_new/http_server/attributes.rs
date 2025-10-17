use std::fmt::Debug;
use std::net::SocketAddr;

use http::Uri;
use http::header::FORWARDED;
use http::header::USER_AGENT;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::CLIENT_ADDRESS;
use opentelemetry_semantic_conventions::trace::CLIENT_PORT;
use opentelemetry_semantic_conventions::trace::HTTP_ROUTE;
use opentelemetry_semantic_conventions::trace::SERVER_ADDRESS;
use opentelemetry_semantic_conventions::trace::SERVER_PORT;
use opentelemetry_semantic_conventions::trace::URL_PATH;
use opentelemetry_semantic_conventions::trace::URL_QUERY;
use opentelemetry_semantic_conventions::trace::URL_SCHEME;
use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::axum_factory::utils::ConnectionInfo;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::NETWORK_LOCAL_ADDRESS;
use crate::plugins::telemetry::config_new::attributes::NETWORK_LOCAL_PORT;
use crate::plugins::telemetry::config_new::attributes::NETWORK_PEER_ADDRESS;
use crate::plugins::telemetry::config_new::attributes::NETWORK_PEER_PORT;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::router;
use crate::services::router::Request;

/// Attributes for Http servers
/// See https://opentelemetry.io/docs/specs/semconv/http/http-spans/#http-server
#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct HttpServerAttributes {
    /// Client address - domain name if available without reverse DNS lookup, otherwise IP address or Unix domain socket name.
    /// Examples:
    ///
    /// * 83.164.160.102
    ///
    /// Requirement level: Recommended
    #[serde(rename = "client.address", skip)]
    pub(crate) client_address: Option<StandardAttribute>,
    /// The port of the original client behind all proxies, if known (e.g. from Forwarded or a similar header). Otherwise, the immediate client peer port.
    /// Examples:
    ///
    /// * 65123
    ///
    /// Requirement level: Recommended
    #[serde(rename = "client.port", skip)]
    pub(crate) client_port: Option<StandardAttribute>,
    /// The matched route (path template in the format used by the respective server framework).
    /// Examples:
    ///
    /// * /graphql
    ///
    /// Requirement level: Conditionally Required: If and only if it’s available
    #[serde(rename = "http.route")]
    pub(crate) http_route: Option<StandardAttribute>,
    /// Local socket address. Useful in case of a multi-IP host.
    /// Examples:
    ///
    /// * 10.1.2.80
    /// * /tmp/my.sock
    ///
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.address")]
    pub(crate) network_local_address: Option<StandardAttribute>,
    /// Local socket port. Useful in case of a multi-port host.
    /// Examples:
    ///
    /// * 65123
    ///
    /// Requirement level: Opt-In
    #[serde(rename = "network.local.port")]
    pub(crate) network_local_port: Option<StandardAttribute>,
    /// Peer address of the network connection - IP address or Unix domain socket name.
    /// Examples:
    ///
    /// * 10.1.2.80
    /// * /tmp/my.sock
    ///
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.address")]
    pub(crate) network_peer_address: Option<StandardAttribute>,
    /// Peer port number of the network connection.
    /// Examples:
    ///
    /// * 65123
    ///
    /// Requirement level: Recommended
    #[serde(rename = "network.peer.port")]
    pub(crate) network_peer_port: Option<StandardAttribute>,
    /// Name of the local HTTP server that received the request.
    /// Examples:
    ///
    /// * example.com
    /// * 10.1.2.80
    /// * /tmp/my.sock
    ///
    /// Requirement level: Recommended
    #[serde(rename = "server.address")]
    pub(crate) server_address: Option<StandardAttribute>,
    /// Port of the local HTTP server that received the request.
    /// Examples:
    ///
    /// * 80
    /// * 8080
    /// * 443
    ///
    /// Requirement level: Recommended
    #[serde(rename = "server.port")]
    pub(crate) server_port: Option<StandardAttribute>,
    /// The URI path component
    /// Examples:
    ///
    /// * /search
    ///
    /// Requirement level: Required
    #[serde(rename = "url.path")]
    pub(crate) url_path: Option<StandardAttribute>,
    /// The URI query component
    /// Examples:
    ///
    /// * q=OpenTelemetry
    ///
    /// Requirement level: Conditionally Required: If and only if one was received/sent.
    #[serde(rename = "url.query")]
    pub(crate) url_query: Option<StandardAttribute>,

    /// The URI scheme component identifying the used protocol.
    /// Examples:
    ///
    /// * http
    /// * https
    ///
    /// Requirement level: Required
    #[serde(rename = "url.scheme")]
    pub(crate) url_scheme: Option<StandardAttribute>,

    /// Value of the HTTP User-Agent header sent by the client.
    /// Examples:
    ///
    /// * CERN-LineMode/2.15
    /// * libwww/2.17b3
    ///
    /// Requirement level: Recommended
    #[serde(rename = "user_agent.original")]
    pub(crate) user_agent_original: Option<StandardAttribute>,
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
                        self.url_scheme = Some(StandardAttribute::Bool(true));
                    }
                    if self.url_path.is_none() {
                        self.url_path = Some(StandardAttribute::Bool(true));
                    }
                    if self.url_query.is_none() {
                        self.url_query = Some(StandardAttribute::Bool(true));
                    }

                    if self.http_route.is_none() {
                        self.http_route = Some(StandardAttribute::Bool(true));
                    }
                }
                TelemetryDataKind::Metrics => {
                    if self.server_address.is_none() {
                        self.server_address = Some(StandardAttribute::Bool(true));
                    }
                    if self.server_port.is_none() && self.server_address.is_some() {
                        self.server_port = Some(StandardAttribute::Bool(true));
                    }
                }
            },
            DefaultAttributeRequirementLevel::Recommended => match kind {
                TelemetryDataKind::Traces => {
                    if self.client_address.is_none() {
                        self.client_address = Some(StandardAttribute::Bool(true));
                    }
                    if self.server_address.is_none() {
                        self.server_address = Some(StandardAttribute::Bool(true));
                    }
                    if self.server_port.is_none() && self.server_address.is_some() {
                        self.server_port = Some(StandardAttribute::Bool(true));
                    }
                    if self.user_agent_original.is_none() {
                        self.user_agent_original = Some(StandardAttribute::Bool(true));
                    }
                }
                TelemetryDataKind::Metrics => {}
            },
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors<router::Request, router::Response, ()> for HttpServerAttributes {
    fn on_request(&self, request: &router::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self
            .http_route
            .as_ref()
            .and_then(|a| a.key(HTTP_ROUTE.into()))
        {
            attrs.push(KeyValue::new(
                key,
                request.router_request.uri().path().to_string(),
            ));
        }
        if let Some(key) = self
            .client_address
            .as_ref()
            .and_then(|a| a.key(CLIENT_ADDRESS.into()))
        {
            if let Some(forwarded) = Self::forwarded_for(request) {
                attrs.push(KeyValue::new(key, forwarded.ip().to_string()));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
                && let Some(socket) = connection_info.peer_address
            {
                attrs.push(KeyValue::new(key, socket.ip().to_string()));
            }
        }
        if let Some(key) = self
            .client_port
            .as_ref()
            .and_then(|a| a.key(CLIENT_PORT.into()))
        {
            if let Some(forwarded) = Self::forwarded_for(request) {
                attrs.push(KeyValue::new(key, forwarded.port() as i64));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
                && let Some(socket) = connection_info.peer_address
            {
                attrs.push(KeyValue::new(key, socket.port() as i64));
            }
        }

        if let Some(key) = self
            .server_address
            .as_ref()
            .and_then(|a| a.key(SERVER_ADDRESS.into()))
        {
            if let Some(forwarded) =
                Self::forwarded_host(request).and_then(|h| h.host().map(|h| h.to_string()))
            {
                attrs.push(KeyValue::new(key, forwarded));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
                && let Some(socket) = connection_info.server_address
            {
                attrs.push(KeyValue::new(key, socket.ip().to_string()));
            }
        }
        if let Some(key) = self
            .server_port
            .as_ref()
            .and_then(|a| a.key(SERVER_PORT.into()))
        {
            if let Some(forwarded) = Self::forwarded_host(request).and_then(|h| h.port_u16()) {
                attrs.push(KeyValue::new(key, forwarded as i64));
            } else if let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
                && let Some(socket) = connection_info.server_address
            {
                attrs.push(KeyValue::new(key, socket.port() as i64));
            }
        }

        if let Some(key) = self
            .network_local_address
            .as_ref()
            .and_then(|a| a.key(NETWORK_LOCAL_ADDRESS))
            && let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            && let Some(socket) = connection_info.server_address
        {
            attrs.push(KeyValue::new(key, socket.ip().to_string()));
        }
        if let Some(key) = self
            .network_local_port
            .as_ref()
            .and_then(|a| a.key(NETWORK_LOCAL_PORT))
            && let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            && let Some(socket) = connection_info.server_address
        {
            attrs.push(KeyValue::new(key, socket.port() as i64));
        }

        if let Some(key) = self
            .network_peer_address
            .as_ref()
            .and_then(|a| a.key(NETWORK_PEER_ADDRESS))
            && let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            && let Some(socket) = connection_info.peer_address
        {
            attrs.push(KeyValue::new(key, socket.ip().to_string()));
        }
        if let Some(key) = self
            .network_peer_port
            .as_ref()
            .and_then(|a| a.key(NETWORK_PEER_PORT))
            && let Some(connection_info) =
                request.router_request.extensions().get::<ConnectionInfo>()
            && let Some(socket) = connection_info.peer_address
        {
            attrs.push(KeyValue::new(key, socket.port() as i64));
        }

        let router_uri = request.router_request.uri();
        if let Some(key) = self.url_path.as_ref().and_then(|a| a.key(URL_PATH.into())) {
            attrs.push(KeyValue::new(key, router_uri.path().to_string()));
        }
        if let Some(key) = self
            .url_query
            .as_ref()
            .and_then(|a| a.key(URL_QUERY.into()))
            && let Some(query) = router_uri.query()
        {
            attrs.push(KeyValue::new(key, query.to_string()));
        }
        if let Some(key) = self
            .url_scheme
            .as_ref()
            .and_then(|a| a.key(URL_SCHEME.into()))
            && let Some(scheme) = router_uri.scheme_str()
        {
            attrs.push(KeyValue::new(key, scheme.to_string()));
        }
        if let Some(key) = self
            .user_agent_original
            .as_ref()
            .and_then(|a| a.key(USER_AGENT_ORIGINAL.into()))
            && let Some(user_agent) = request
                .router_request
                .headers()
                .get(&USER_AGENT)
                .and_then(|h| h.to_str().ok())
        {
            attrs.push(KeyValue::new(key, user_agent.to_string()));
        }

        attrs
    }

    fn on_response(&self, _response: &router::Response) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
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

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use http::HeaderValue;
    use http::Uri;
    use http::header::FORWARDED;
    use http::header::USER_AGENT;
    use opentelemetry_semantic_conventions::trace::CLIENT_ADDRESS;
    use opentelemetry_semantic_conventions::trace::CLIENT_PORT;
    use opentelemetry_semantic_conventions::trace::HTTP_ROUTE;
    use opentelemetry_semantic_conventions::trace::NETWORK_TYPE;
    use opentelemetry_semantic_conventions::trace::SERVER_ADDRESS;
    use opentelemetry_semantic_conventions::trace::SERVER_PORT;
    use opentelemetry_semantic_conventions::trace::URL_PATH;
    use opentelemetry_semantic_conventions::trace::URL_QUERY;
    use opentelemetry_semantic_conventions::trace::URL_SCHEME;
    use opentelemetry_semantic_conventions::trace::USER_AGENT_ORIGINAL;

    use crate::axum_factory::utils::ConnectionInfo;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_LOCAL_ADDRESS;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_LOCAL_PORT;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_PEER_ADDRESS;
    use crate::plugins::telemetry::config_new::attributes::NETWORK_PEER_PORT;
    use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
    use crate::plugins::telemetry::config_new::http_common::attributes::HttpCommonAttributes;
    use crate::plugins::telemetry::config_new::http_server::attributes::HttpServerAttributes;
    use crate::services::router;

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

    #[test]
    fn test_http_server_client_address() {
        let server = HttpServerAttributes {
            client_address: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == CLIENT_ADDRESS)
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
                .find(|key_val| key_val.key.as_str() == CLIENT_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"2.4.6.8".into())
        );
    }

    #[test]
    fn test_http_server_client_port() {
        let server = HttpServerAttributes {
            client_port: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == CLIENT_PORT)
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
                .find(|key_val| key_val.key.as_str() == CLIENT_PORT)
                .map(|key_val| &key_val.value),
            Some(&8000.into())
        );
    }

    #[test]
    fn test_http_server_http_route() {
        let server = HttpServerAttributes {
            http_route: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == HTTP_ROUTE)
                .map(|key_val| &key_val.value),
            Some(&"/graphql".into())
        );
    }

    #[test]
    fn test_http_server_network_local_address() {
        let server = HttpServerAttributes {
            network_local_address: Some(StandardAttribute::Bool(true)),
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
            network_local_port: Some(StandardAttribute::Bool(true)),
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
            network_peer_address: Some(StandardAttribute::Bool(true)),
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
            network_peer_port: Some(StandardAttribute::Bool(true)),
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
            server_address: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == SERVER_ADDRESS)
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
                .find(|key_val| key_val.key.as_str() == SERVER_ADDRESS)
                .map(|key_val| &key_val.value),
            Some(&"2.4.6.8".into())
        );
    }

    #[test]
    fn test_http_server_server_port() {
        let server = HttpServerAttributes {
            server_port: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == SERVER_PORT)
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
                .find(|key_val| key_val.key.as_str() == SERVER_PORT)
                .map(|key_val| &key_val.value),
            Some(&8000.into())
        );
    }
    #[test]
    fn test_http_server_url_path() {
        let server = HttpServerAttributes {
            url_path: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == URL_PATH)
                .map(|key_val| &key_val.value),
            Some(&"/graphql".into())
        );
    }
    #[test]
    fn test_http_server_query() {
        let server = HttpServerAttributes {
            url_query: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == URL_QUERY)
                .map(|key_val| &key_val.value),
            Some(&"hi=5".into())
        );
    }
    #[test]
    fn test_http_server_scheme() {
        let server = HttpServerAttributes {
            url_scheme: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == URL_SCHEME)
                .map(|key_val| &key_val.value),
            Some(&"https".into())
        );
    }

    #[test]
    fn test_http_server_user_agent_original() {
        let server = HttpServerAttributes {
            user_agent_original: Some(StandardAttribute::Bool(true)),
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
                .find(|key_val| key_val.key.as_str() == USER_AGENT_ORIGINAL)
                .map(|key_val| &key_val.value),
            Some(&"my-agent".into())
        );
    }
}
