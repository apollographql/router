use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::request::coerce_variable_values;
use apollo_compiler::response::GraphQLError;
use apollo_compiler::response::JsonMap;
use apollo_compiler::response::JsonValue;
use apollo_compiler::validation::Valid;
use apollo_router::_private::new_subgraph_response;
use apollo_router::graphql;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::services::subgraph;
use tower::BoxError;
use tower::ServiceExt;

use self::execution::resolver::ResolvedValue;

mod execution;

apollo_router::register_plugin!("apollo", "subgraph_mocks", SubgraphMocksPlugin);

/// Example of inline `router.yaml` config:
///
/// ```yaml
/// subgraph_mocks:
///   subgraph1_name:
///     query:
///       rootField:
///         subField: "value"
///     entities:
///       - __typename: Something
///         id: 4
///         field: [42, 7]
/// ```
///
/// Example with mocked data in separate `mocked_subgraph_data.yaml` file:
/// TODO: what’s the base path for this relative filename?
///
/// ```yaml
/// subgraph_mocks: ${file.mocked_subgraph_data.yaml}
/// ```
type Config = HashMap<String, Arc<SubgraphConfig>>;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SubgraphConfig {
    #[serde(default)]
    #[schemars(with = "HashMap<String, String>")]
    headers: HeaderMap,
    #[serde(default)]
    #[schemars(with = "OtherJsonMap")]
    query: JsonMap,
    #[serde(default)]
    #[schemars(with = "Option<OtherJsonMap>")]
    mutation: Option<JsonMap>,
    #[serde(default)]
    #[schemars(with = "Vec<OtherJsonMap>")]
    entities: Vec<JsonMap>,
}

type OtherJsonMap = serde_json::Map<String, serde_json::Value>;

#[derive(Default)]
struct HeaderMap(http::HeaderMap);

struct SubgraphMocksPlugin {
    per_subgraph_config: Config,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<Schema>>>>,
}

#[async_trait::async_trait]
impl Plugin for SubgraphMocksPlugin {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            subgraph_schemas: apollo_router::_private::plugin_init_subgraph_schemas(&init).clone(),
            per_subgraph_config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, _service: subgraph::BoxService) -> subgraph::BoxService {
        let config = self.per_subgraph_config[name].clone();
        let subgraph_schema = self.subgraph_schemas[name].clone();
        tower::service_fn(move |request: subgraph::Request| {
            let config = config.clone();
            let subgraph_schema = subgraph_schema.clone();
            async move {
                let body =
                    subgraph_call(&config, &subgraph_schema, request.subgraph_request.body())
                        .unwrap_or_else(|e| {
                            graphql::Response::builder()
                                .errors(e.into_iter().map(Into::into).collect())
                                .build()
                        });
                let mut response = http::Response::builder();
                *response.headers_mut().unwrap() = config.headers.0.clone();
                let response = response.body(body).unwrap();
                Ok(new_subgraph_response(request, response))
            }
        })
        .boxed()
    }
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
    let variable_values = coerce_variable_values(subgraph_schema, operation, &request.variables)
        .map_err(|e| vec![e.to_graphql_error(&doc.sources)])?;
    let object_type_name = operation.object_type();
    let plain_error = |message: &str| vec![GraphQLError::new(message, None, &doc.sources)];
    let root_operation_object_type_def = subgraph_schema
        .get_object(object_type_name)
        .ok_or_else(|| plain_error("undefined root operation object type"))?;
    let (mode, root_mocks) = match operation.operation_type {
        OperationType::Query => (execution::engine::ExecutionMode::Normal, &config.query),
        OperationType::Mutation => (
            execution::engine::ExecutionMode::Sequential,
            config
                .mutation
                .as_ref()
                .ok_or_else(|| plain_error("mutation is not supported"))?,
        ),
        OperationType::Subscription => return Err(plain_error("subscription not supported")),
    };
    let initial_value = RootResolver {
        root_mocks,
        entities: &config.entities,
    };
    let mut errors = Vec::new();
    let path = None;
    let data = match execution::engine::execute_selection_set(
        subgraph_schema,
        &doc,
        &variable_values,
        &mut errors,
        path,
        mode,
        root_operation_object_type_def,
        &initial_value,
        &operation.selection_set.selections,
    ) {
        Ok(map) => JsonValue::Object(map),
        Err(execution::engine::PropagateNull) => JsonValue::Null,
    };
    Ok(graphql::Response::builder()
        .data(data)
        .errors(errors.into_iter().map(Into::into).collect())
        .extensions(JsonMap::new())
        .build())
}

struct RootResolver<'a> {
    root_mocks: &'a JsonMap,
    entities: &'a [JsonMap],
}

struct MockResolver<'a> {
    mocks: &'a JsonMap,
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

impl execution::resolver::Resolver for RootResolver<'_> {
    fn type_name(&self) -> &str {
        unreachable!()
    }

    fn resolve_field<'a>(
        &'a self,
        field_name: &'a str,
        arguments: &'a JsonMap,
    ) -> Result<ResolvedValue<'a>, execution::resolver::ResolverError> {
        if field_name != "_entities" {
            return resolve_normal_field(self.root_mocks, field_name, arguments);
        }
        let entities = arguments["representations"]
            .as_array()
            .ok_or("expected array `representations`")?
            .iter()
            .map(|representation| {
                let representation = representation
                    .as_object()
                    .ok_or("expected object `representations[n]`")?;
                let entity = self.find_entities(representation).ok_or_else(|| {
                    format!("no mocked entity found for representation {representation:?}")
                })?;
                Ok(ResolvedValue::object(MockResolver { mocks: entity }))
            });
        Ok(ResolvedValue::list(entities))
    }
}

impl execution::resolver::Resolver for MockResolver<'_> {
    fn type_name(&self) -> &str {
        self.mocks
            .get("__typename")
            .expect("missing `__typename` mock for interface or union type")
            .as_str()
            .expect("`__typename` is not a string")
    }

    fn resolve_field<'a>(
        &'a self,
        field_name: &'a str,
        arguments: &'a JsonMap,
    ) -> Result<ResolvedValue<'a>, execution::resolver::ResolverError> {
        resolve_normal_field(self.mocks, field_name, arguments)
    }
}

fn resolve_normal_field<'a>(
    mocks: &'a JsonMap,
    field_name: &'a str,
    arguments: &'a JsonMap,
) -> Result<ResolvedValue<'a>, execution::resolver::ResolverError> {
    if !arguments.is_empty() {
        return Err("arguments not supported".into()); // TODO?
    }
    let mock = mocks
        .get(field_name)
        .ok_or("field not found in mocked data")?;
    resolve_value(mock)
}

fn resolve_value(mock: &JsonValue) -> Result<ResolvedValue<'_>, String> {
    match mock {
        JsonValue::Object(map) => Ok(ResolvedValue::object(MockResolver { mocks: map })),
        JsonValue::Array(values) => Ok(ResolvedValue::list(values.iter().map(resolve_value))),
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
