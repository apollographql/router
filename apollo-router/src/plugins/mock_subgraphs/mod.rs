use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::resolvers::Execution;
use apollo_compiler::resolvers::FieldError;
use apollo_compiler::resolvers::ObjectValue;
use apollo_compiler::resolvers::ResolveInfo;
use apollo_compiler::resolvers::ResolvedValue;
use apollo_compiler::response::GraphQLError;
use apollo_compiler::response::JsonMap;
use apollo_compiler::response::JsonValue;
use apollo_compiler::validation::Valid;
use tower::BoxError;
use tower::ServiceExt;

use crate::graphql;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::response_cache::plugin::GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS;
use crate::plugins::response_cache::plugin::GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS;
use crate::services::subgraph;

register_private_plugin!("apollo", "experimental_mock_subgraphs", MockSubgraphsPlugin);

/// Configuration for the `mock_subgraphs` plugin
///
///
/// Example `router.yaml`:
///
/// ```yaml
/// experimental_mock_subgraphs:
///   subgraph1_name:
///     headers:
///       cache-control: public
///     query:
///       rootField:
///         subField: "value"
///         __cacheTags: ["rootField"]
///     entities:
///       - __typename: Something
///         id: 4
///         field: [42, 7]
///         __cacheTags: ["something-4"]
/// ```
//
// If changing this, also update `dev-docs/mock_subgraphs_plugin.md`
type Config = HashMap<String, Arc<SubgraphConfig>>;

/// Configuration for one subgraph for the `mock_subgraphs` plugin
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SubgraphConfig {
    // If changing this struct, also update `dev-docs/mock_subgraphs_plugin.md`
    /// HTTP headers for the subgraph response
    #[serde(default)]
    #[schemars(with = "HashMap<String, String>")]
    headers: HeaderMap,

    /// Data for `query` operations (excluding the special `_entities` field)
    ///
    /// In maps nested in this one (but not at the top level), the `__cacheTags` key is special.
    /// Instead of representing a field that can be selected, when its parent field is selected
    /// its value is expected to be an array which is appended
    /// to the `response.extensions["apolloCacheTags"]` array.
    #[serde(default)]
    #[schemars(with = "OtherJsonMap")]
    query: JsonMap,

    /// Data for `mutation` operations
    #[serde(default)]
    #[schemars(with = "Option<OtherJsonMap>")]
    mutation: Option<JsonMap>,

    /// Entities that can be queried through Federation’s special `_entities` field
    ///
    /// In maps directly in the top-level `Vec` (but not in other maps nested deeper),
    /// the `__cacheTags` key is special.
    /// Instead of representing a field that can be selected, when its parent entity is selected
    /// its contents are added to the `response.extensions["apolloEntityCacheTags"]` array.
    #[serde(default)]
    #[schemars(with = "Vec<OtherJsonMap>")]
    entities: Vec<JsonMap>,
}

type OtherJsonMap = serde_json::Map<String, serde_json::Value>;

#[derive(Default)]
struct HeaderMap(http::HeaderMap);

// Exposed this way for the test harness, so the plugin type itself doesn't need to be made pub.
pub(crate) static PLUGIN_NAME: LazyLock<&'static str> =
    LazyLock::new(std::any::type_name::<MockSubgraphsPlugin>);

struct MockSubgraphsPlugin {
    per_subgraph_config: Config,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<Schema>>>>,
}

#[async_trait::async_trait]
impl PluginPrivate for MockSubgraphsPlugin {
    type Config = Config;

    const HIDDEN_FROM_CONFIG_JSON_SCHEMA: bool = true;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            subgraph_schemas: init.subgraph_schemas.clone(),
            per_subgraph_config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, _: subgraph::BoxService) -> subgraph::BoxService {
        let config = self.per_subgraph_config.get(name).cloned();
        let subgraph_schema = self.subgraph_schemas[name].clone();
        tower::service_fn(move |request: subgraph::Request| {
            let config = config.clone();
            let subgraph_schema = subgraph_schema.clone();
            async move {
                let mut response = http::Response::builder();
                let body = if let Some(config) = &config {
                    *response.headers_mut().unwrap() = config.headers.0.clone();
                    subgraph_call(config, &subgraph_schema, request.subgraph_request.body())
                        .unwrap_or_else(|e| {
                            graphql::Response::builder()
                                .errors(e.into_iter().map(Into::into).collect())
                                .build()
                        })
                } else {
                    graphql::Response::builder()
                        .error(
                            graphql::Error::builder()
                                .message("subgraph mock not configured")
                                .extension_code("SUBGRAPH_MOCK_NOT_CONFIGURED")
                                .build(),
                        )
                        .build()
                };
                let response = response.body(body).unwrap();
                Ok(subgraph::Response::new_from_response(
                    response,
                    request.context,
                    request.subgraph_name,
                    request.id,
                ))
            }
        })
        .boxed()
    }
}

/// Entry point for testing this mock
pub fn testing_subgraph_call(
    config: JsonValue,
    subgraph_schema: &Valid<Schema>,
    request: &graphql::Request,
) -> Result<graphql::Response, Vec<GraphQLError>> {
    let config = serde_json_bytes::from_value(config).unwrap();
    subgraph_call(&config, subgraph_schema, request)
}

fn subgraph_call(
    config: &SubgraphConfig,
    subgraph_schema: &Valid<Schema>,
    request: &graphql::Request,
) -> Result<graphql::Response, Vec<GraphQLError>> {
    let query = request.query.as_deref().unwrap_or("");
    let doc = ExecutableDocument::parse_and_validate(subgraph_schema, query, "query")
        .map_err(|e| e.errors.iter().map(|e| e.to_json()).collect::<Vec<_>>())?;
    let operation = doc
        .operations
        .get(request.operation_name.as_deref())
        .map_err(|e| vec![e.to_graphql_error(&doc.sources)])?;

    let plain_error = |message: &str| vec![GraphQLError::new(message, None, &doc.sources)];
    let root_mocks = match operation.operation_type {
        OperationType::Query => &config.query,
        OperationType::Mutation => config
            .mutation
            .as_ref()
            .ok_or_else(|| plain_error("mutation is not supported"))?,
        OperationType::Subscription => return Err(plain_error("subscription not supported")),
    };
    let response_extensions = RefCell::new(JsonMap::new());
    let initial_value = RootResolver {
        root_mocks,
        entities: &config.entities,
        response_extensions: &response_extensions,
    };
    let result = Execution::new(subgraph_schema, &doc)
        .operation(operation)
        .raw_variable_values(&request.variables)
        .execute_sync(&initial_value);
    match result {
        Ok(response) => Ok(graphql::Response::builder()
            .data(Some(JsonValue::from(response.data)))
            .errors(response.errors.into_iter().map(Into::into).collect())
            .extensions(response_extensions.into_inner())
            .build()),
        Err(request_error) => Err(vec![request_error.to_graphql_error(&doc.sources)]),
    }
}

struct RootResolver<'a> {
    root_mocks: &'a JsonMap,
    entities: &'a [JsonMap],
    response_extensions: &'a RefCell<JsonMap>,
}

struct MockResolver<'a> {
    type_name: &'a str,
    in_entity: bool,
    mocks: &'a JsonMap,
    response_extensions: &'a RefCell<JsonMap>,
}

impl<'a> RootResolver<'a> {
    fn find_entities(&self, representation: &JsonMap) -> Option<&'a JsonMap> {
        self.entities.iter().find(|entity| {
            representation
                .iter()
                .all(|(k, v)| entity.get(k).is_some_and(|value| value == v))
        })
    }
}

impl ObjectValue for RootResolver<'_> {
    fn type_name(&self) -> &str {
        unreachable!()
    }

    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        if info.field_name() != "_entities" {
            let in_entity = false;
            return resolve_normal_field(
                self.response_extensions,
                in_entity,
                self.root_mocks,
                info,
            );
        }
        let entities = info.arguments()["representations"]
            .as_array()
            .ok_or(FieldError {
                message: "expected array `representations`".into(),
            })?
            .iter()
            .map(move |representation| {
                let representation = representation.as_object().ok_or(FieldError {
                    message: "expected object `representations[n]`".into(),
                })?;
                let entity = self
                    .find_entities(representation)
                    .ok_or_else(|| FieldError {
                        message: format!(
                            "no mocked entity found for representation {representation:?}"
                        ),
                    })?;
                if let Some(keys) = entity.get("__cacheTags") {
                    self.response_extensions
                        .borrow_mut()
                        .entry(GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS)
                        .or_insert_with(|| JsonValue::Array(Vec::new()))
                        .as_array_mut()
                        .unwrap()
                        .push(keys.clone());
                }
                Ok(ResolvedValue::object(MockResolver {
                    type_name: mock_type_name(entity)
                        .expect("missing `__typename` mock for entity"),
                    in_entity: true,
                    mocks: entity,
                    response_extensions: self.response_extensions,
                }))
            });
        Ok(ResolvedValue::List(Box::new(entities)))
    }
}

impl ObjectValue for MockResolver<'_> {
    fn type_name(&self) -> &str {
        self.type_name
    }

    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        resolve_normal_field(self.response_extensions, self.in_entity, self.mocks, info)
    }
}

fn mock_type_name(mock: &JsonMap) -> Option<&str> {
    Some(
        mock.get("__typename")?
            .as_str()
            .expect("`__typename` is not a string"),
    )
}

fn resolve_normal_field<'a>(
    response_extensions: &'a RefCell<JsonMap>,
    in_entity: bool,
    mocks: &'a JsonMap,
    info: &'a ResolveInfo<'a>,
) -> Result<ResolvedValue<'a>, FieldError> {
    // TODO: find some way to vary response based on arguments?
    let field_name = info.field_name();
    let mock = mocks.get(field_name).ok_or_else(|| FieldError {
        message: format!("field '{field_name}' not found in mocked data"),
    })?;
    resolve_value(response_extensions, in_entity, mock, info)
}

fn resolve_value<'a>(
    response_extensions: &'a RefCell<JsonMap>,
    in_entity: bool,
    mock: &'a JsonValue,
    info: &'a ResolveInfo<'a>,
) -> Result<ResolvedValue<'a>, FieldError> {
    match mock {
        JsonValue::Object(map) => {
            if !in_entity && let Some(keys) = map.get("__cacheTags") {
                response_extensions
                    .borrow_mut()
                    .entry(GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS)
                    .or_insert_with(|| JsonValue::Array(Vec::new()))
                    .as_array_mut()
                    .unwrap()
                    .extend_from_slice(keys.as_array().unwrap());
            };
            Ok(ResolvedValue::object(MockResolver {
                type_name: mock_type_name(map)
                    .unwrap_or_else(|| info.field_definition().ty.inner_named_type()),
                in_entity,
                mocks: map,
                response_extensions,
            }))
        }
        JsonValue::Array(values) => {
            Ok(ResolvedValue::List(Box::new(values.iter().map(move |x| {
                resolve_value(response_extensions, in_entity, x, info)
            }))))
        }
        json => Ok(ResolvedValue::leaf(json.clone())),
    }
}

impl<'de> serde::Deserialize<'de> for HeaderMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let mut map = http::HeaderMap::new();
        for (k, v) in <HashMap<String, String>>::deserialize(deserializer)? {
            map.insert(
                http::HeaderName::from_bytes(k.as_bytes()).map_err(D::Error::custom)?,
                http::HeaderValue::from_str(&v).map_err(D::Error::custom)?,
            );
        }
        Ok(Self(map))
    }
}
