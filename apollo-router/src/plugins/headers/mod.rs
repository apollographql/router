use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use http::HeaderMap;
use http::HeaderValue;
use http::header::ACCEPT;
use http::header::ACCEPT_ENCODING;
use http::header::CONNECTION;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::header::HOST;
use http::header::HeaderName;
use http::header::PROXY_AUTHENTICATE;
use http::header::PROXY_AUTHORIZATION;
use http::header::TE;
use http::header::TRAILER;
use http::header::TRANSFER_ENCODING;
use http::header::UPGRADE;
use itertools::Itertools;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use serde_json_bytes::path::JsonPathInst;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugin::serde::deserialize_jsonpath;
use crate::plugin::serde::deserialize_option_header_name;
use crate::plugin::serde::deserialize_option_header_value;
use crate::plugin::serde::deserialize_regex;
use crate::services::SubgraphRequest;
use crate::services::connector;
use crate::services::subgraph;

register_private_plugin!("apollo", "headers", Headers);

#[derive(Clone, JsonSchema, Deserialize, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HeadersLocation {
    /// Propagate/Insert/Remove headers from request
    request: Vec<Operation>,
    // Propagate/Insert/Remove headers from response
    // response: Option<Operation>
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Insert(Insert),
    Remove(Remove),
    Propagate(Propagate),
}

schemar_fn!(remove_named, String, "Remove a header given a header name");
schemar_fn!(
    remove_matching,
    String,
    "Remove a header given a regex matching against the header name"
);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Remove header
enum Remove {
    #[schemars(schema_with = "remove_named")]
    #[serde(deserialize_with = "deserialize_header_name")]
    /// Remove a header given a header name
    Named(HeaderName),

    #[schemars(schema_with = "remove_matching")]
    #[serde(deserialize_with = "deserialize_regex")]
    /// Remove a header given a regex matching header name
    Matching(Regex),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Insert header
enum Insert {
    /// Insert static header
    Static(InsertStatic),
    /// Insert header with a value coming from context key (works only for a string in the context)
    FromContext(InsertFromContext),
    /// Insert header with a value coming from body
    FromBody(InsertFromBody),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert static header
struct InsertStatic {
    /// The name of the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The value for the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_value")]
    value: HeaderValue,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert header with a value coming from context key
struct InsertFromContext {
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    /// Specify header name
    name: HeaderName,
    /// Specify context key to fetch value
    from_context: String,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert header with a value coming from body
struct InsertFromBody {
    /// The target header name
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The path in the request body
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_jsonpath")]
    path: JsonPathInst,

    /// The default if the path in the body did not resolve to an element
    #[schemars(with = "Option<String>", default)]
    #[serde(deserialize_with = "deserialize_option_header_value", default)]
    default: Option<HeaderValue>,
}

schemar_fn!(
    propagate_matching,
    String,
    "Remove a header given a regex matching header name"
);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde(untagged)]
/// Propagate header
enum Propagate {
    /// Propagate header given a header name
    Named {
        /// The source header name
        #[schemars(with = "String")]
        #[serde(deserialize_with = "deserialize_header_name")]
        named: HeaderName,

        /// An optional target header name
        #[schemars(with = "Option<String>", default)]
        #[serde(deserialize_with = "deserialize_option_header_name", default)]
        rename: Option<HeaderName>,

        /// Default value for the header.
        #[schemars(with = "Option<String>", default)]
        #[serde(deserialize_with = "deserialize_option_header_value", default)]
        default: Option<HeaderValue>,
    },
    /// Propagate header given a regex to match header name
    Matching {
        /// The regex on header name
        #[schemars(schema_with = "propagate_matching")]
        #[serde(deserialize_with = "deserialize_regex")]
        matching: Regex,
    },
}

#[derive(Clone, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct ConnectorHeadersConfiguration {
    /// Map of subgraph_name.connector_source_name to configuration
    #[serde(default)]
    sources: HashMap<String, HeadersLocation>,

    /// Options applying to all sources across all subgraphs
    #[serde(default)]
    all: Option<HeadersLocation>,
}

/// Configuration for header propagation
#[derive(Clone, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
#[schemars(rename = "HeadersConfig")]
struct Config {
    /// Rules to apply to all subgraphs
    all: Option<HeadersLocation>,
    /// Rules to specific subgraphs
    subgraphs: HashMap<String, HeadersLocation>,
    /// Rules for connectors
    connector: ConnectorHeadersConfiguration,
}

struct Headers {
    all_operations: Arc<Vec<Operation>>,
    subgraph_operations: HashMap<String, Arc<Vec<Operation>>>,
    all_connector_operations: Arc<Vec<Operation>>,
    connector_source_operations: HashMap<String, Arc<Vec<Operation>>>,
}

#[async_trait::async_trait]
impl PluginPrivate for Headers {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let operations: Vec<Operation> = init
            .config
            .all
            .as_ref()
            .map(|a| a.request.clone())
            .unwrap_or_default();
        let subgraph_operations = init
            .config
            .subgraphs
            .iter()
            .map(|(subgraph_name, op)| {
                let mut operations = operations.clone();
                operations.append(&mut op.request.clone());
                (subgraph_name.clone(), Arc::new(operations))
            })
            .collect();
        let all_connector_operations: Vec<Operation> = init
            .config
            .connector
            .all
            .as_ref()
            .map(|a| a.request.clone())
            .unwrap_or_default();
        let connector_source_operations = init
            .config
            .connector
            .sources
            .iter()
            .map(|(subgraph_name, op)| {
                let mut operations = operations.clone();
                operations.append(&mut op.request.clone());
                (subgraph_name.clone(), Arc::new(operations))
            })
            .collect();

        Ok(Headers {
            all_operations: Arc::new(operations),
            all_connector_operations: Arc::new(all_connector_operations),
            subgraph_operations,
            connector_source_operations,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        ServiceBuilder::new()
            .layer(HeadersLayer::new(
                self.subgraph_operations
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| self.all_operations.clone()),
            ))
            .service(service)
            .boxed()
    }

    fn connector_request_service(
        &self,
        service: crate::services::connector::request_service::BoxService,
        source_name: String,
    ) -> crate::services::connector::request_service::BoxService {
        ServiceBuilder::new()
            .layer(HeadersLayer::new(
                self.connector_source_operations
                    .get(&source_name)
                    .cloned()
                    .unwrap_or_else(|| self.all_connector_operations.clone()),
            ))
            .service(service)
            .boxed()
    }
}

struct HeadersLayer {
    operations: Arc<Vec<Operation>>,
}

impl HeadersLayer {
    fn new(operations: Arc<Vec<Operation>>) -> Self {
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
    operations: Arc<Vec<Operation>>,
}

// Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
// These are not propagated by default using a regex match as they will not make sense for the
// second hop.
// In addition because our requests are not regular proxy requests content-type, content-length
// and host are also in the exclude list.
static RESERVED_HEADERS: [HeaderName; 14] = [
    CONNECTION,
    PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION,
    TE,
    TRAILER,
    TRANSFER_ENCODING,
    UPGRADE,
    CONTENT_LENGTH,
    CONTENT_TYPE,
    CONTENT_ENCODING,
    HOST,
    ACCEPT,
    ACCEPT_ENCODING,
    HeaderName::from_static("keep-alive"),
];

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
        self.modify_subgraph_request(&mut req);
        self.inner.call(req)
    }
}

impl<S> Service<connector::request_service::Request> for HeadersService<S>
where
    S: Service<connector::request_service::Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: connector::request_service::Request) -> Self::Future {
        self.modify_connector_request(&mut req);
        self.inner.call(req)
    }
}

impl<S> HeadersService<S> {
    fn modify_subgraph_request(&self, req: &mut SubgraphRequest) {
        let mut already_propagated: HashSet<String> = HashSet::new();

        let body_to_value = serde_json_bytes::value::to_value(req.supergraph_request.body()).ok();
        let supergraph_headers = req.supergraph_request.headers();
        let context = &req.context;
        let headers_mut = req.subgraph_request.headers_mut();

        for operation in &*self.operations {
            operation.process_header_rules(
                &mut already_propagated,
                supergraph_headers,
                &body_to_value,
                context,
                headers_mut,
                None,
            );
        }
    }

    fn modify_connector_request(&self, req: &mut connector::request_service::Request) {
        let mut already_propagated: HashSet<String> = HashSet::new();

        let TransportRequest::Http(ref mut http_request) = req.transport_request;
        let body_to_value = serde_json::from_str(http_request.inner.body()).ok();
        let supergraph_headers = req.supergraph_request.headers();
        let context = &req.context;
        // We need to know what headers were added prior to this processing to that we can properly override as needed
        let existing_headers = http_request.inner.headers().clone();
        let headers_mut = http_request.inner.headers_mut();

        for operation in &*self.operations {
            operation.process_header_rules(
                &mut already_propagated,
                supergraph_headers,
                &body_to_value,
                context,
                headers_mut,
                Some(&existing_headers),
            );
        }
    }
}

impl Operation {
    fn process_header_rules(
        &self,
        already_propagated: &mut HashSet<String>,
        supergraph_headers: &HeaderMap,
        body_to_value: &Option<Value>,
        context: &crate::Context,
        headers_mut: &mut HeaderMap,
        existing_headers: Option<&HeaderMap>,
    ) {
        match self {
            Operation::Insert(insert) => {
                insert.process_header_rules(body_to_value, context, headers_mut)
            }
            Operation::Remove(remove) => remove.process_header_rules(headers_mut),
            Operation::Propagate(propagate) => propagate.process_header_rules(
                already_propagated,
                supergraph_headers,
                headers_mut,
                existing_headers,
            ),
        }
    }
}

impl Insert {
    fn process_header_rules(
        &self,
        body_to_value: &Option<Value>,
        context: &crate::Context,
        headers_mut: &mut HeaderMap,
    ) {
        match self {
            Insert::Static(insert_static) => {
                headers_mut.insert(&insert_static.name, insert_static.value.clone());
            }
            Insert::FromContext(insert_from_context) => {
                if let Some(val) = context
                    .get::<_, String>(&insert_from_context.from_context)
                    .ok()
                    .flatten()
                {
                    match HeaderValue::from_str(&val) {
                        Ok(header_value) => {
                            headers_mut.insert(&insert_from_context.name, header_value);
                        }
                        Err(err) => {
                            tracing::error!(
                                "cannot convert from the context into a header value for header name '{}': {:?}",
                                insert_from_context.name,
                                err
                            );
                        }
                    }
                }
            }
            Insert::FromBody(from_body) => {
                if let Some(body_to_value) = &body_to_value {
                    let output = from_body.path.find(body_to_value);
                    if let serde_json_bytes::Value::Null = output {
                        if let Some(default_val) = &from_body.default {
                            headers_mut.insert(&from_body.name, default_val.clone());
                        }
                    } else {
                        let header_value = if let serde_json_bytes::Value::String(val_str) = output
                        {
                            val_str.as_str().to_string()
                        } else {
                            output.to_string()
                        };
                        match HeaderValue::from_str(&header_value) {
                            Ok(header_value) => {
                                headers_mut.insert(&from_body.name, header_value);
                            }
                            Err(err) => {
                                let header_name = &from_body.name;
                                tracing::error!(%header_name, ?err, "cannot convert from the body into a header value for header name");
                            }
                        }
                    }
                } else if let Some(default_val) = &from_body.default {
                    headers_mut.insert(&from_body.name, default_val.clone());
                }
            }
        }
    }
}

impl Remove {
    fn process_header_rules(&self, headers_mut: &mut HeaderMap) {
        match self {
            Remove::Named(name) => {
                headers_mut.remove(name);
            }
            Remove::Matching(matching) => {
                let new_headers = headers_mut
                    .drain()
                    .filter_map(|(name, value)| {
                        name.and_then(|name| {
                            (RESERVED_HEADERS.contains(&name) || !matching.is_match(name.as_str()))
                                .then_some((name, value))
                        })
                    })
                    .collect();

                let _ = std::mem::replace(headers_mut, new_headers);
            }
        }
    }
}

impl Propagate {
    fn process_header_rules(
        &self,
        already_propagated: &mut HashSet<String>,
        supergraph_headers: &HeaderMap,
        headers_mut: &mut HeaderMap,
        existing_headers: Option<&HeaderMap>,
    ) {
        let default_headers = Default::default();
        let existing_headers = existing_headers.unwrap_or(&default_headers);
        match self {
            Propagate::Named {
                named,
                rename,
                default,
            } => {
                let target_header = rename.as_ref().unwrap_or(named);
                if !already_propagated.contains(target_header.as_str()) {
                    // If the header was already added previously by some other
                    // method (e.g Connectors), remove it first before propagating
                    // the value from the client request. This allows us to use
                    // `.append` instead of `.insert` to handle multiple headers.
                    //
                    // Note: Rhai and Coprocessor plugins run after this plugin,
                    // so this will not remove headers added there.
                    if existing_headers.contains_key(target_header) {
                        headers_mut.remove(target_header);
                    }

                    let values = supergraph_headers.get_all(named);
                    if values.iter().count() == 0 {
                        if let Some(default) = default {
                            headers_mut.append(target_header, default.clone());
                            already_propagated.insert(target_header.to_string());
                        }
                    } else {
                        for value in values {
                            headers_mut.append(target_header, value.clone());
                            already_propagated.insert(target_header.to_string());
                        }
                    }
                }
            }
            Propagate::Matching { matching } => {
                supergraph_headers
                    .iter()
                    .filter(|(name, _)| {
                        !RESERVED_HEADERS.contains(*name) && matching.is_match(name.as_str())
                    })
                    .chunk_by(|(name, ..)| name.to_owned())
                    .into_iter()
                    .for_each(|(name, headers)| {
                        if !already_propagated.contains(name.as_str()) {
                            // If the header was already added previously by some other
                            // method (e.g Connectors), remove it first before propagating
                            // the value from the client request. This allows us to use
                            // `.append` instead of `.insert` to handle multiple headers.
                            //
                            // Note: Rhai and Coprocessor plugins run after this plugin,
                            // so this will not remove headers added there.
                            if existing_headers.contains_key(name) {
                                headers_mut.remove(name);
                            }

                            headers.for_each(|(_, value)| {
                                headers_mut.append(name, value.clone());
                            });
                            already_propagated.insert(name.to_string());
                        }
                    });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::str::FromStr;
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use serde_json_bytes::json;
    use subgraph::SubgraphRequestId;
    use tower::BoxError;

    use super::*;
    use crate::Context;
    use crate::graphql;
    use crate::graphql::Request;
    use crate::plugin::test::MockConnectorService;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugins::test::PluginTestHarness;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

    #[test]
    fn test_subgraph_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            request:
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
            request:
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
            request:
                - remove:
                    named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                - remove:
                    matching: "d.*"
        "#,
        )
        .unwrap();

        assert!(
            serde_yaml::from_str::<Config>(
                r#"
        all:
            request:
                - remove:
                    matching: "d.*["
        "#,
            )
            .is_err()
        );
    }

    #[test]
    fn test_propagate_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                - propagate:
                    named: "test"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
                - propagate:
                    named: "test"
                    rename: "bif"
        "#,
        )
        .unwrap();

        serde_yaml::from_str::<Config>(
            r#"
        all:
            request:
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
            request:
                - propagate:
                    matching: "d.*"
        "#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_insert_static() -> Result<(), BoxError> {
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

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::Static(
            InsertStatic {
                name: "c".try_into()?,
                value: "d".try_into()?,
            },
        ))]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_static() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
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
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::Static(
            InsertStatic {
                name: "c".try_into()?,
                value: "d".try_into()?,
            },
        ))]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_context() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_context", "my_value_from_context"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(
            Insert::FromContext(InsertFromContext {
                name: "header_from_context".try_into()?,
                from_context: "my_key".to_string(),
            }),
        )]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_from_context() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_context", "my_value_from_context"),
                ])
            })
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(
            Insert::FromContext(InsertFromContext {
                name: "header_from_context".try_into()?,
                from_context: "my_key".to_string(),
            }),
        )]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_request_body() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_request", "my_operation_name"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str("$.operationName").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_from_request_body() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_request", "myCoolValue"),
                ])
            })
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str("$.myCoolField").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_from_request_body_with_old_access_json_notation() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_request", "my_operation_name"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str(".operationName").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_insert_from_request_body_with_old_access_json_notation()
    -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("header_from_request", "myCoolValue"),
                ])
            })
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Insert(Insert::FromBody(
            InsertFromBody {
                name: "header_from_request".try_into()?,
                path: JsonPathInst::from_str(".myCoolField").unwrap(),
                default: None,
            },
        ))]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_exact() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]))
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_exact_multiple() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]))
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        let req = SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa") // will be removed
                .header("aa", "vaa") // will be removed
                .header("aa", "vaa2") // will be removed
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: ctx,
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
        };

        service.ready().await?.call(req).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_remove_exact() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac"), ("ab", "vab")]))
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Named(
            "aa".try_into()?,
        ))]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_matching() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac")]))
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_remove_matching() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| request.assert_headers(vec![("ac", "vac")]))
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![Operation::Remove(Remove::Matching(
            Regex::from_str("a[ab]")?,
        ))]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
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
                    ("db", "vdb2"),
                ])
            })
            .returning(example_response);

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str("d[ab]")?,
            })]))
            .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_matching() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("da", "vda"),
                    ("db", "vdb"),
                    ("db", "vdb2"),
                ])
            })
            .returning(example_connector_response);

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str("d[ab]")?,
            })]))
            .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: None,
                default: None,
            })]))
            .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_exact() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
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
            .returning(example_connector_response);

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: None,
                default: None,
            })]))
            .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ea".try_into()?),
                default: None,
            })]))
            .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connect_propagate_exact_rename() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
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
            .returning(example_connector_response);

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ea".try_into()?),
                default: None,
            })]))
            .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_multiple() -> Result<(), BoxError> {
        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("ra", "vda"),
                    ("rb", "vda"),
                ])
            })
            .returning(example_response);

        let mut service = HeadersLayer::new(Arc::new(vec![
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("rb".try_into()?),
                default: None,
            }),
            // This should not take effect as the header is already propagated
            Operation::Propagate(Propagate::Named {
                named: "db".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
        ]))
        .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_multiple() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                request.assert_headers(vec![
                    ("aa", "vaa"),
                    ("ab", "vab"),
                    ("ac", "vac"),
                    ("ra", "vda"),
                    ("rb", "vda"),
                ])
            })
            .returning(example_connector_response);

        let mut service = HeadersLayer::new(Arc::new(vec![
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
            Operation::Propagate(Propagate::Named {
                named: "da".try_into()?,
                rename: Some("rb".try_into()?),
                default: None,
            }),
            // This should not take effect as the header is already propagated
            Operation::Propagate(Propagate::Named {
                named: "db".try_into()?,
                rename: Some("ra".try_into()?),
                default: None,
            }),
        ]))
        .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
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

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "ea".try_into()?,
                rename: None,
                default: Some("defaulted".try_into()?),
            })]))
            .layer(mock);

        service.ready().await?.call(example_request()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_connector_propagate_exact_default() -> Result<(), BoxError> {
        let mut mock = MockConnectorService::new();
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
            .returning(example_connector_response);

        let mut service =
            HeadersLayer::new(Arc::new(vec![Operation::Propagate(Propagate::Named {
                named: "ea".try_into()?,
                rename: None,
                default: Some("defaulted".try_into()?),
            })]))
            .layer(mock);

        service
            .ready()
            .await?
            .call(example_connector_request())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_reserved() -> Result<(), BoxError> {
        let service = HeadersService {
            inner: MockSubgraphService::new(),
            operations: Arc::new(vec![Operation::Propagate(Propagate::Matching {
                matching: Regex::from_str(".*")?,
            })]),
        };

        let mut request = SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .header(CONTENT_ENCODING, "identity")
                    .header(ACCEPT, "application/json")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
        };
        service.modify_subgraph_request(&mut request);
        let headers = request
            .subgraph_request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            headers,
            vec![
                ("aa", "vaa"),
                ("ab", "vab"),
                ("ac", "vac"),
                ("host", "rhost"),
                ("content-length", "22"),
                ("content-type", "graphql"),
                ("da", "vda"),
                ("db", "vdb"),
                ("db", "vdb"),
                ("db", "vdb2"),
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_propagate_multiple_matching_rules() -> Result<(), BoxError> {
        let service = HeadersService {
            inner: MockSubgraphService::new(),
            operations: Arc::new(vec![
                Operation::Propagate(Propagate::Named {
                    named: HeaderName::from_static("dc"),
                    rename: None,
                    default: None,
                }),
                Operation::Propagate(Propagate::Matching {
                    matching: Regex::from_str("dc")?,
                }),
            ]),
        };

        let mut request = SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("dc", "vdb2")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
        };
        service.modify_subgraph_request(&mut request);
        let headers = request
            .subgraph_request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            headers,
            vec![("aa", "vaa"), ("ab", "vab"), ("ac", "vac"), ("dc", "vdb2"),]
        );

        Ok(())
    }

    fn example_response(req: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse::new_from_response(
            http::Response::default(),
            Context::new(),
            req.subgraph_name,
            SubgraphRequestId(String::new()),
        ))
    }

    fn example_connector_response(
        _req: connector::request_service::Request,
    ) -> Result<connector::request_service::Response, BoxError> {
        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };
        Ok(connector::request_service::Response::test_new(
            key,
            Vec::new(),
            json!(""),
            None,
        ))
    }

    fn example_request() -> SubgraphRequest {
        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header("aa", "vaa")
                .header("ab", "vab")
                .header("ac", "vac")
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: ctx,
            subgraph_name: String::from("test"),
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: SubgraphRequestId(String::new()),
        }
    }

    fn example_connector_request() -> connector::request_service::Request {
        let ctx = Context::new();
        ctx.insert("my_key", "my_value_from_context".to_string())
            .unwrap();
        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(a),
                None,
                0,
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("f").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        };
        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };

        let request = http::Request::builder()
            .header("aa", "vaa")
            .header("ab", "vab")
            .header("ac", "vac")
            .header(HOST, "rhost")
            .header(CONTENT_LENGTH, "22")
            .header(CONTENT_TYPE, "graphql")
            .body(
                json!({
                    "myCoolField": "myCoolValue"
                })
                .to_string(),
            )
            .unwrap();

        let http_request = HttpRequest {
            inner: request,
            debug: Default::default(),
        };

        connector::request_service::Request {
            context: ctx,
            connector: Arc::new(connector),
            transport_request: http_request.into(),
            key,
            mapping_problems: Default::default(),
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header("da", "vda")
                    .header("db", "vdb")
                    .header("db", "vdb")
                    .header("db", "vdb2")
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
        }
    }

    impl SubgraphRequest {
        fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let actual_headers = self
                .subgraph_request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }

    impl connector::request_service::Request {
        fn assert_headers(&self, headers: Vec<(&'static str, &'static str)>) -> bool {
            let mut headers = headers.clone();
            headers.push((HOST.as_str(), "rhost"));
            headers.push((CONTENT_LENGTH.as_str(), "22"));
            headers.push((CONTENT_TYPE.as_str(), "graphql"));
            let TransportRequest::Http(ref http_request) = self.transport_request;
            let actual_headers = http_request
                .inner
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_str().unwrap()))
                .collect::<HashSet<_>>();
            assert_eq!(actual_headers, headers.into_iter().collect::<HashSet<_>>());

            true
        }
    }

    async fn assert_headers(
        config: &'static str,
        input: Vec<(&'static str, &'static str)>,
        output: Vec<(&'static str, &'static str)>,
    ) {
        let test_harness = PluginTestHarness::<Headers>::builder()
            .config(config)
            .build()
            .await
            .expect("test harness");
        let service = test_harness.subgraph_service("test", move |r| {
            let output = output.clone();
            async move {
                // Assert the headers here
                let headers = r.subgraph_request.headers();
                for (name, value) in output.iter() {
                    if let Some(header) = headers.get(*name) {
                        assert_eq!(header.to_str().unwrap(), *value);
                    } else {
                        panic!("missing header {name}");
                    }
                }
                Ok(subgraph::Response::fake_builder().build())
            }
        });

        let mut req = http::Request::builder();
        for (name, value) in input.iter() {
            req = req.header(*name, *value);
        }

        service
            .call(
                subgraph::Request::fake_builder()
                    .supergraph_request(Arc::new(
                        req.body(graphql::Request::default())
                            .expect("valid request"),
                    ))
                    .build(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_propagate_passthrough() {
        assert_headers(
            include_str!("fixtures/propagate_passthrough.router.yaml"),
            vec![("a", "av"), ("c", "cv")],
            vec![("a", "av"), ("b", "av"), ("c", "cv")],
        )
        .await;

        assert_headers(
            include_str!("fixtures/propagate_passthrough.router.yaml"),
            vec![("b", "bv"), ("c", "cv")],
            vec![("b", "bv"), ("c", "cv")],
        )
        .await;
    }

    #[tokio::test]
    async fn test_propagate_passthrough_defaulted() {
        assert_headers(
            include_str!("fixtures/propagate_passthrough_defaulted.router.yaml"),
            vec![("a", "av")],
            vec![("b", "av")],
        )
        .await;

        assert_headers(
            include_str!("fixtures/propagate_passthrough_defaulted.router.yaml"),
            vec![("b", "bv")],
            vec![("b", "bv")],
        )
        .await;
        assert_headers(
            include_str!("fixtures/propagate_passthrough_defaulted.router.yaml"),
            vec![("c", "cv")],
            vec![("b", "defaulted")],
        )
        .await;
    }
}
