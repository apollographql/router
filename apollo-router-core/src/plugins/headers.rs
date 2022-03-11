use crate::plugin::Plugin;
use crate::{register_plugin, SubgraphRequest, SubgraphResponse};
use http::header::{
    HeaderName, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use http::HeaderValue;
use lazy_static::lazy_static;
use regex::Regex;
use schemars::JsonSchema;
use serde::de::{Error, Visitor};
use serde::{de, Deserialize, Deserializer};
use std::collections::HashMap;
use std::fmt::Formatter;
use std::str::FromStr;
use std::task::{Context, Poll};
use tower::util::BoxService;
use tower::{BoxError, Layer, ServiceBuilder, ServiceExt};
use tower_service::Service;

register_plugin!("apollo", "headers", Headers);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Insert(Insert),
    Remove(Remove),
    Propagate(Propagate),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Remove {
    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_header_name")]
    Name(HeaderName),

    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_regex")]
    Matching(Regex),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Insert {
    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,
    #[schemars(schema_with = "string_schema")]
    #[serde(deserialize_with = "deserialize_header_value")]
    value: HeaderValue,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
enum Propagate {
    Matching {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },

    Named {
        #[schemars(schema_with = "string_schema")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,
        #[schemars(schema_with = "option_string_schema")]
        #[serde(deserialize_with = "deserialize_option_header_name")]
        rename: Option<HeaderName>,
        #[schemars(schema_with = "option_string_schema")]
        #[serde(deserialize_with = "deserialize_option_header_value")]
        default: Option<HeaderValue>,
    },
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    all: Vec<Operation>,
    #[serde(default)]
    subgraphs: HashMap<String, Vec<Operation>>,
}

struct Headers {
    config: Config,
}

fn string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    String::json_schema(gen)
}

fn option_string_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<String>::json_schema(gen)
}

impl Plugin for Headers {
    type Config = Config;

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(Headers { config })
    }
    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let mut operations = self.config.all.clone();
        if let Some(subgraph_operations) = self.config.subgraphs.get(name) {
            operations.append(&mut subgraph_operations.clone())
        }

        ServiceBuilder::new()
            .layer(HeadersLayer::new(operations))
            .service(service)
            .boxed()
    }
}

struct HeadersLayer {
    operations: Vec<Operation>,
}

impl HeadersLayer {
    fn new(operations: Vec<Operation>) -> Self {
        Self { operations }
    }
}

impl<S> Layer<S> for HeadersLayer {
    type Service = HeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HeadersService {
            inner,
            operations: self.operations.clone(),
        }
    }
}
struct HeadersService<S> {
    inner: S,
    operations: Vec<Operation>,
}

lazy_static! {
    // Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
    // These are not propagated by default using a regex match as they will not make sense for the
    // second hop.
    // In addition because our requests are not regular proxy requests content-type, content-length
    // and host are also in the exclude list.
    static ref RESERVED_HEADERS: Vec<HeaderName> = [
        CONNECTION,
        PROXY_AUTHENTICATE,
        PROXY_AUTHORIZATION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
        CONTENT_LENGTH,
        CONTENT_TYPE,
        HOST,
        HeaderName::from_static("keep-alive")
    ]
    .into();
}

impl<S> Service<SubgraphRequest> for HeadersService<S>
where
    S: Service<SubgraphRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
        for operation in &self.operations {
            match operation {
                Operation::Insert(config) => {
                    req.http_request
                        .headers_mut()
                        .insert(&config.name, config.value.clone());
                }
                Operation::Remove(Remove::Name(name)) => {
                    req.http_request.headers_mut().remove(name);
                }
                Operation::Remove(Remove::Matching(matching)) => {
                    let headers = req.http_request.headers_mut();
                    let matching_headers = headers
                        .iter()
                        .filter_map(|(name, _)| {
                            matching.is_match(name.as_str()).then(|| name.clone())
                        })
                        .filter(|name| !RESERVED_HEADERS.contains(name))
                        .collect::<Vec<_>>();
                    for name in matching_headers {
                        headers.remove(name);
                    }
                }
                Operation::Propagate(Propagate::Named {
                    named,
                    rename,
                    default,
                }) => {
                    let headers = req.http_request.headers_mut();
                    let value = req.context.request.headers().get(named);
                    if let Some(value) = value.or(default.as_ref()) {
                        headers.insert(rename.as_ref().unwrap_or(named), value.clone());
                    }
                }
                Operation::Propagate(Propagate::Matching { matching }) => {
                    let headers = req.http_request.headers_mut();
                    req.context
                        .request
                        .headers()
                        .iter()
                        .filter(|(name, _)| matching.is_match(name.as_str()))
                        .filter(|(name, _)| !RESERVED_HEADERS.contains(name))
                        .for_each(|(name, value)| {
                            headers.insert(name, value.clone());
                        });
                }
            }
        }
        self.inner.call(req)
    }
}

// We may want to eventually pull these serializers out
fn deserialize_option_header_name<'de, D>(deserializer: D) -> Result<Option<HeaderName>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionHeaderNameVisitor;

    impl<'de> Visitor<'de> for OptionHeaderNameVisitor {
        type Value = Option<HeaderName>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct HeaderName")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: de::Deserializer<'de>,
        {
            Ok(Some(deserializer.deserialize_str(HeaderNameVisitor)?))
        }
    }
    deserializer.deserialize_option(OptionHeaderNameVisitor)
}

fn deserialize_option_header_value<'de, D>(deserializer: D) -> Result<Option<HeaderValue>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionHeaderValueVisitor;

    impl<'de> Visitor<'de> for OptionHeaderValueVisitor {
        type Value = Option<HeaderValue>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct HeaderValue")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: de::Deserializer<'de>,
        {
            Ok(Some(deserializer.deserialize_str(HeaderValueVisitor)?))
        }
    }

    deserializer.deserialize_option(OptionHeaderValueVisitor)
}

struct HeaderNameVisitor;

impl<'de> Visitor<'de> for HeaderNameVisitor {
    type Value = HeaderName;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("struct HeaderName")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        HeaderName::try_from(v).map_err(|e| de::Error::custom(format!("Invalid header name {}", e)))
    }
}

fn deserialize_header_name<'de, D>(deserializer: D) -> Result<HeaderName, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(HeaderNameVisitor)
}

struct HeaderValueVisitor;

impl<'de> Visitor<'de> for HeaderValueVisitor {
    type Value = HeaderValue;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("struct HeaderValue")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        HeaderValue::try_from(v)
            .map_err(|e| de::Error::custom(format!("Invalid header value {}", e)))
    }
}

fn deserialize_header_value<'de, D>(deserializer: D) -> Result<HeaderValue, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(HeaderValueVisitor)
}

fn deserialize_regex<'de, D>(deserializer: D) -> Result<Regex, D::Error>
where
    D: Deserializer<'de>,
{
    struct RegexVisitor;

    impl<'de> Visitor<'de> for RegexVisitor {
        type Value = Regex;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("struct Regex")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: Error,
        {
            Regex::from_str(v).map_err(|e| de::Error::custom(format!("{}", e)))
        }
    }
    deserializer.deserialize_str(RegexVisitor)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::fetch::OperationKind;
    use crate::http_compat::RequestBuilder;
    use crate::plugin_utils::MockSubgraphService;
    use crate::plugins::headers::{Config, HeadersLayer};
    use crate::{Context, Request, Response, SubgraphRequest, SubgraphResponse};
    use http::Method;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tower::BoxError;
    use url::Url;

    #[test]
    fn test_subgraph_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            - insert:
                name: "test"
                value: "test"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_insert_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            - insert:
                name: "test"
                value: "test"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_remove_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            - remove:
                name: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            - remove:
                matching: "d.*"
        "#,
        )
        .unwrap();

        assert!(serde_yaml::from_str::<Config>(
            r#"
        all:
            - remove:
                matching: "d.*["
        "#,
        )
        .is_err());
    }

    #[test]
    fn test_propagate_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            - propagate:
                named: "test"
                rename: "bif"
                default: "bof"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            - propagate:
                matching: "d.*"
        "#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_insert() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("c", "d"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Insert(Insert {
            name: "c".try_into()?,
            value: "d".try_into()?,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_exact() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]))
            .returning(example_response);

        let mut service =
            HeadersLayer::new(vec![Operation::Remove(Remove::Name("aa".try_into()?))])
                .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_matching() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac")]))
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_matching() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("da", "vda"),
                    ("db", "vdb"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Matching {
            matching: Regex::from_str("d[ab]")?,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("da", "vda"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Named {
            named: "da".try_into()?,
            rename: None,
            default: None,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact_rename() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("ea", "vda"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Named {
            named: "da".try_into()?,
            rename: Some("ea".try_into()?),
            default: None,
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_exact_default() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("ea", "defaulted"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(vec![Operation::Propagate(Propagate::Named {
            named: "ea".try_into()?,
            rename: None,
            default: Some("defaulted".try_into()?),
        })])
        .layer(mock.build());

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    fn example_response(_: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse {
            response: http::Response::builder()
                .body(Response::builder().build())
                .unwrap()
                .into(),
            context: example_originating_request(),
        })
    }

    fn example_originating_request() -> Context {
        Context::new().with_request(Arc::new(
            RequestBuilder::new(Method::GET, Url::parse("http://test").unwrap())
                .header("da", "vda")
                .header("db", "vdb")
                .header("dc", "vdc")
                .header(HOST, "host")
                .header(CONTENT_LENGTH, "2")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .unwrap(),
        ))
    }

    fn example_request() -> SubgraphRequest {
        SubgraphRequest {
            http_request: RequestBuilder::new(Method::GET, Url::parse("http://test").unwrap())
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .unwrap(),
            operation_kind: OperationKind::Query,
            context: example_originating_request(),
        }
    }

    impl SubgraphRequest {
        pub fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let actual_headers = self
                .http_request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }
}
