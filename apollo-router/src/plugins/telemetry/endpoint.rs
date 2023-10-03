use std::fmt::Formatter;
use std::net::SocketAddr;
use std::str::FromStr;

use http::uri::Authority;
use http::Uri;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::JsonSchema;
use serde::de::Error;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Deserializer;

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct UriEndpoint {
    // None means that the value `default` was specified. We may remove the use of `default` in the future.
    uri: Option<Uri>,
}

impl UriEndpoint {
    /// Converts an endpoint to a URI using the default endpoint as reference for any URI parts that are missing.
    pub(crate) fn to_uri(&self, default_endpoint: &Uri) -> Option<Uri> {
        self.uri.as_ref().map(|uri| {
            let mut parts = uri.clone().into_parts();
            if parts.scheme.is_none() {
                parts.scheme = default_endpoint.scheme().cloned();
            }

            match (&parts.authority, default_endpoint.authority()) {
                (None, Some(default_authority)) => {
                    parts.authority = Some(default_authority.clone());
                }
                (Some(authority), Some(default_authority)) => {
                    let host = if authority.host().is_empty() {
                        default_authority.host()
                    } else {
                        authority.host()
                    };
                    let port = if authority.port().is_none() {
                        default_authority.port()
                    } else {
                        authority.port()
                    };

                    if let Some(port) = port {
                        parts.authority = Some(
                            Authority::from_str(format!("{}:{}", host, port).as_str())
                                .expect("host and port must have come from a valid uri, qed"),
                        )
                    } else {
                        parts.authority = Some(
                            Authority::from_str(host)
                                .expect("host must have come from a valid uri, qed"),
                        );
                    }
                }
                _ => {}
            }

            if parts.path_and_query.is_none() {
                parts.path_and_query = default_endpoint.path_and_query().cloned();
            }

            Uri::from_parts(parts)
                .expect("uri cannot be invalid as it was constructed from existing parts")
        })
    }
}

impl<'de> Deserialize<'de> for UriEndpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EndpointVisitor;

        impl<'de> Visitor<'de> for EndpointVisitor {
            type Value = UriEndpoint;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("a valid uri or 'default'")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if v == "default" {
                    // This is a legacy of the old config format, where the 'default' was accepted.
                    // Users should just not set the endpoint if they want the default.
                    return Ok(UriEndpoint::default());
                }
                match Uri::from_str(v) {
                    Ok(uri) => Ok(UriEndpoint { uri: Some(uri) }),
                    Err(_) => Err(Error::custom(format!(
                        "invalid endpoint: {}. Expected a valid uri or 'default'",
                        v
                    ))),
                }
            }
        }

        deserializer.deserialize_str(EndpointVisitor)
    }
}

impl JsonSchema for UriEndpoint {
    fn schema_name() -> String {
        "UriEndpoint".to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        gen.subschema_for::<String>()
    }
}

impl From<Uri> for UriEndpoint {
    fn from(uri: Uri) -> Self {
        UriEndpoint { uri: Some(uri) }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct SocketEndpoint {
    // None means that the value `default` was specified. We may remove the use of `default` in the future.
    socket: Option<SocketAddr>,
}

impl SocketEndpoint {
    pub(crate) fn to_socket(&self) -> Option<SocketAddr> {
        self.socket
    }
}

impl<'de> Deserialize<'de> for SocketEndpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EndpointVisitor;

        impl<'de> Visitor<'de> for EndpointVisitor {
            type Value = SocketEndpoint;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("a valid uri or 'default'")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if v == "default" {
                    // This is a legacy of the old config format, where the 'default' was accepted.
                    // Users should just not set the endpoint if they want the default.
                    return Ok(SocketEndpoint::default());
                }
                match SocketAddr::from_str(v) {
                    Ok(socket) => Ok(SocketEndpoint {
                        socket: Some(socket),
                    }),
                    Err(_) => Err(Error::custom(format!(
                        "invalid endpoint: {}. Expected a valid socket or 'default'",
                        v
                    ))),
                }
            }
        }

        deserializer.deserialize_str(EndpointVisitor)
    }
}

impl JsonSchema for SocketEndpoint {
    fn schema_name() -> String {
        "SocketEndpoint".to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        gen.subschema_for::<String>()
    }
}

impl From<SocketAddr> for SocketEndpoint {
    fn from(socket: SocketAddr) -> Self {
        SocketEndpoint {
            socket: Some(socket),
        }
    }
}

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use http::Uri;

    use crate::plugins::telemetry::endpoint::SocketEndpoint;
    use crate::plugins::telemetry::endpoint::UriEndpoint;

    #[test]
    fn test_parse_uri_default() {
        let endpoint = serde_yaml::from_str::<UriEndpoint>("default").unwrap();
        assert_eq!(endpoint, UriEndpoint::default());
    }
    #[test]
    fn test_parse_uri() {
        let endpoint = serde_yaml::from_str::<UriEndpoint>("http://example.com:2000/path").unwrap();
        assert_eq!(
            endpoint,
            Uri::from_static("http://example.com:2000/path").into()
        );
    }

    #[test]
    fn test_parse_uri_error() {
        let error = serde_yaml::from_str::<UriEndpoint>("example.com:2000/path")
            .expect_err("expected error");
        assert_eq!(error.to_string(), "invalid endpoint: example.com:2000/path. Expected a valid uri or 'default' at line 1 column 1");
    }

    #[test]
    fn test_to_url() {
        assert_eq!(
            UriEndpoint::from(Uri::from_static("example.com"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://example.com:9411/path2")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("example.com:2000"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://example.com:2000/path2")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("http://example.com:2000/"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://example.com:2000/")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("http://example.com:2000/path1"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://example.com:2000/path1")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("http://example.com:2000"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://example.com:2000")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("http://example.com/path1"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://example.com:9411/path1")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("http://:2000/path1"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://localhost:2000/path1")
        );
        assert_eq!(
            UriEndpoint::from(Uri::from_static("/path1"))
                .to_uri(&Uri::from_static("http://localhost:9411/path2"))
                .unwrap(),
            Uri::from_static("http://localhost:9411/path1")
        );
    }

    #[test]
    fn test_parse_socket_default() {
        let endpoint = serde_yaml::from_str::<SocketEndpoint>("default").unwrap();
        assert_eq!(endpoint, SocketEndpoint::default());
    }
    #[test]
    fn test_parse_socket() {
        let endpoint = serde_yaml::from_str::<SocketEndpoint>("127.0.0.1:8000").unwrap();
        assert_eq!(
            endpoint,
            SocketAddr::from_str("127.0.0.1:8000").unwrap().into()
        );
    }

    #[test]
    fn test_parse_socket_error() {
        let error = serde_yaml::from_str::<SocketEndpoint>("example.com:2000/path")
            .expect_err("expected error");
        assert_eq!(error.to_string(), "invalid endpoint: example.com:2000/path. Expected a valid socket or 'default' at line 1 column 1");
    }

    #[test]
    fn test_to_socket() {
        assert_eq!(
            SocketEndpoint::from(SocketAddr::from_str("127.0.0.1:8000").unwrap())
                .to_socket()
                .unwrap(),
            SocketAddr::from_str("127.0.0.1:8000").unwrap()
        );
    }
}
