use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::ast::OperationType;
use apollo_compiler::request::coerce_variable_values;
use apollo_compiler::response::GraphQLError;
use apollo_compiler::response::JsonMap;
use apollo_compiler::response::JsonValue;
use apollo_compiler::validation::Valid;
use tower::BoxError;
use tower::ServiceExt;

use self::execution::resolver::ResolvedValue;
use crate::graphql;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::response_cache::plugin::GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS;
use crate::plugins::response_cache::plugin::GRAPHQL_RESPONSE_EXTENSION_ROOT_FIELDS_CACHE_TAGS;
use crate::services::subgraph;

pub(crate) mod execution;

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
    let response_extensions = RefCell::new(JsonMap::new());
    let path = None;
    let data = match execution::engine::execute_selection_set(
        subgraph_schema,
        &doc,
        &variable_values,
        &mut errors,
        &response_extensions,
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
        .extensions(response_extensions.into_inner())
        .build())
}

struct RootResolver<'a> {
    root_mocks: &'a JsonMap,
    entities: &'a [JsonMap],
}

struct MockResolver<'a> {
    in_entity: bool,
    mocks: &'a JsonMap,
}

impl<'a> RootResolver<'a> {
    fn find_entities(&self, representation: &JsonMap) -> Option<&'a JsonMap> {
        self.entities.iter().find(|entity| {
            representation.iter().all(|(k, v)| {
                entity
                    .get(k)
                    .is_some_and(|value| Self::is_a_match(value, v))
            })
        })
    }

    /// Tests whether an entity and its representation are a match with support for complex
    /// keys
    ///
    /// Supports:
    ///  * simple scalars: @key(fields: "id"), where id is an ID
    ///  * complex scalars: @key(fields: "productId userId"), where both are IDs
    ///  * nested objects in complex keys: @key(fields: "id items { id name }"), where items is an
    ///    array of a type that nested fields
    fn is_a_match(entity_value: &JsonValue, representation_value: &JsonValue) -> bool {
        match (entity_value, representation_value) {
            // strings, numbers, bools, and null are directly comparable via straightforward
            // equality
            (JsonValue::String(a), JsonValue::String(b)) => a == b,
            (JsonValue::Number(a), JsonValue::Number(b)) => a == b,
            (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
            (JsonValue::Null, JsonValue::Null) => true,

            // anything with nested fields needs to recurse down until we have something directly
            // comparable
            (JsonValue::Object(entity_obj), JsonValue::Object(repr_obj)) => {
                repr_obj.iter().all(|(k, v)| {
                    entity_obj
                        .get(k)
                        .is_some_and(|entity_field_value| Self::is_a_match(entity_field_value, v))
                })
            }

            // arrays might be filled with values that have nested fields
            (JsonValue::Array(a), JsonValue::Array(b)) => {
                // early return arrays of different lengths
                a.len() == b.len()
                    // for all A values
                    && a.iter().all(|av|{
                    // and any B value, if there's a match, return true
                    b.iter().any(|bv| {
                        Self::is_a_match(av, bv)
                    })
                })
            }

            // no match found
            _ => false,
        }
    }
}

impl execution::resolver::Resolver for RootResolver<'_> {
    fn type_name(&self) -> &str {
        unreachable!()
    }

    fn resolve_field<'a>(
        &'a self,
        response_extensions: &'a RefCell<JsonMap>,
        field_name: &'a str,
        arguments: &'a JsonMap,
    ) -> Result<ResolvedValue<'a>, execution::resolver::ResolverError> {
        if field_name != "_entities" {
            let in_entity = false;
            return resolve_normal_field(
                response_extensions,
                in_entity,
                self.root_mocks,
                field_name,
                arguments,
            );
        }
        let entities = arguments["representations"]
            .as_array()
            .ok_or("expected array `representations`")?
            .iter()
            .map(move |representation| {
                let representation = representation
                    .as_object()
                    .ok_or("expected object `representations[n]`")?;
                let entity = self.find_entities(representation).ok_or_else(|| {
                    format!("no mocked entity found for representation {representation:?}")
                })?;
                if let Some(keys) = entity.get("__cacheTags") {
                    response_extensions
                        .borrow_mut()
                        .entry(GRAPHQL_RESPONSE_EXTENSION_ENTITY_CACHE_TAGS)
                        .or_insert_with(|| JsonValue::Array(Vec::new()))
                        .as_array_mut()
                        .unwrap()
                        .push(keys.clone());
                }
                Ok(ResolvedValue::object(MockResolver {
                    in_entity: true,
                    mocks: entity,
                }))
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
        response_extensions: &'a RefCell<JsonMap>,
        field_name: &'a str,
        arguments: &'a JsonMap,
    ) -> Result<ResolvedValue<'a>, execution::resolver::ResolverError> {
        resolve_normal_field(
            response_extensions,
            self.in_entity,
            self.mocks,
            field_name,
            arguments,
        )
    }
}

fn resolve_normal_field<'a>(
    response_extensions: &'a RefCell<JsonMap>,
    in_entity: bool,
    mocks: &'a JsonMap,
    field_name: &'a str,
    arguments: &'a JsonMap,
) -> Result<ResolvedValue<'a>, execution::resolver::ResolverError> {
    let _ignored = arguments; // TODO: find some way to vary response based on arguments?
    let mock = mocks
        .get(field_name)
        .ok_or_else(|| format!("field '{field_name}' not found in mocked data"))?;
    resolve_value(response_extensions, in_entity, mock)
}

fn resolve_value<'a>(
    response_extensions: &'a RefCell<JsonMap>,
    in_entity: bool,
    mock: &'a JsonValue,
) -> Result<ResolvedValue<'a>, String> {
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
                in_entity,
                mocks: map,
            }))
        }
        JsonValue::Array(values) => {
            Ok(ResolvedValue::list(values.iter().map(move |x| {
                resolve_value(response_extensions, in_entity, x)
            })))
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

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    #[test]
    fn test_is_a_match_primitives() {
        assert!(RootResolver::is_a_match(&json!("hello"), &json!("hello")));
        assert!(!RootResolver::is_a_match(&json!("hello"), &json!("world")));
        assert!(RootResolver::is_a_match(&json!(42), &json!(42)));
        assert!(!RootResolver::is_a_match(&json!(42), &json!(43)));
        assert!(RootResolver::is_a_match(&json!(true), &json!(true)));
        assert!(!RootResolver::is_a_match(&json!(true), &json!(false)));
        assert!(RootResolver::is_a_match(&json!(null), &json!(null)));
    }

    #[test]
    fn test_is_a_match_type_mismatches() {
        assert!(!RootResolver::is_a_match(&json!("42"), &json!(42)));
        assert!(!RootResolver::is_a_match(&json!(true), &json!(1)));
        assert!(!RootResolver::is_a_match(&json!(null), &json!("")));
    }

    #[test]
    fn test_is_a_match_simple_objects() {
        assert!(RootResolver::is_a_match(
            &json!({"id": "1", "name": "Test"}),
            &json!({"id": "1", "name": "Test"})
        ));

        // entity has extra fields but should match because it has all of the target fields
        // of the representation (what the router thinks identifies the entity)
        assert!(RootResolver::is_a_match(
            &json!({"id": "1", "name": "Test", "extra": "data"}),
            &json!({"id": "1", "name": "Test"})
        ));

        // the representation has more fields than the entity, but since the representation
        // is what the router thinks identifies the entity, this won't count as a match
        assert!(!RootResolver::is_a_match(
            &json!({"id": "1"}),
            &json!({"id": "1", "name": "Test"})
        ));
    }

    #[test]
    fn test_is_a_match_nested_objects() {
        assert!(RootResolver::is_a_match(
            &json!({
                "id": "1",
                "address": {
                    "street": "Main St",
                    "city": "Boston"
                }
            }),
            &json!({
                "id": "1",
                "address": {
                    "street": "Main St",
                    "city": "Boston"
                }
            })
        ));

        // entities with extra fields should match
        assert!(RootResolver::is_a_match(
            &json!({
                "id": "1",
                "address": {
                    "street": "Main St",
                    "city": "Boston",
                    "zip": "02101"
                }
            }),
            &json!({
                "id": "1",
                "address": {
                    "street": "Main St",
                    "city": "Boston"
                }
            })
        ));

        // representations with extra fields shouldn't match; it's what the router thinks
        // identifies the entity
        assert!(!RootResolver::is_a_match(
            &json!({
                "id": "1",
                "address": {
                    "street": "Main St",
                    "city": "Boston",
                    "zip": "02101"
                }
            }),
            &json!({
                "id": "1",
                "address": {
                    "street": "Main St",
                    "city": "Boston",
                    "state": "of mind"
                }
            })
        ));
    }

    #[test]
    fn test_is_a_match_arrays_with_scalars() {
        assert!(RootResolver::is_a_match(
            &json!(["a", "b", "c"]),
            &json!(["a", "b", "c"])
        ));
        assert!(!RootResolver::is_a_match(
            &json!(["a", "b", "c"]),
            &json!(["a", "b", "d"])
        ));
        // different lengths
        assert!(!RootResolver::is_a_match(
            &json!(["a", "b", "c"]),
            &json!(["a", "b"])
        ));
        // order doesn't matter for what identifies an entity
        assert!(RootResolver::is_a_match(
            &json!(["a", "b", "c"]),
            &json!(["a", "c", "b"])
        ));
    }

    #[test]
    fn test_is_a_match_arrays_with_objects() {
        assert!(RootResolver::is_a_match(
            &json!([
                {"id": "1", "name": "Item1"},
                {"id": "2", "name": "Item2"}
            ]),
            &json!([
                {"id": "1", "name": "Item1"},
                {"id": "2", "name": "Item2"}
            ])
        ));
        // extra fields on an entity should match; the representation must match as the
        // identifier
        assert!(RootResolver::is_a_match(
            &json!([
                {"id": "1", "name": "Item1", "price": 10},
                {"id": "2", "name": "Item2", "price": 20}
            ]),
            &json!([
                {"id": "1", "name": "Item1"},
                {"id": "2", "name": "Item2"}
            ])
        ));
        assert!(!RootResolver::is_a_match(
            &json!([
                {"id": "1", "name": "Item1"},
                {"id": "2", "name": "Wrong"}
            ]),
            &json!([
                {"id": "1", "name": "Item1"},
                {"id": "2", "name": "Item2"}
            ])
        ));
    }

    #[test]
    fn test_is_a_match_complex_nested_structures() {
        // complex key scenario: nested objects within arrays should match, even when the
        // entity has more keys than the representation
        assert!(RootResolver::is_a_match(
            &json!({
                "id": "1",
                "items": [
                    {
                        "id": "i1",
                        "name": "Item",
                        "metadata": {
                            "category": "books",
                            "inStock": true
                        }
                    }
                ]
            }),
            &json!({
                "id": "1",
                "items": [
                    {
                        "id": "i1",
                        "name": "Item",
                        "metadata": {
                            "category": "books"
                        }
                    }
                ]
            })
        ));
    }

    #[test]
    fn test_find_entities_simple_match() {
        let entities = vec![
            json!({"__typename": "Product", "id": "1", "name": "Widget"})
                .as_object()
                .unwrap()
                .clone(),
            json!({"__typename": "Product", "id": "2", "name": "Gadget"})
                .as_object()
                .unwrap()
                .clone(),
        ];

        let resolver = RootResolver {
            root_mocks: &JsonMap::new(),
            entities: &entities,
        };

        // find first entity
        let repr_value = json!({"__typename": "Product", "id": "1"});
        let repr = repr_value.as_object().unwrap();
        let result = resolver.find_entities(repr);
        assert_eq!(result.unwrap().get("name").unwrap(), "Widget");

        // find second entity
        let repr_value = json!({"__typename": "Product", "id": "2"});
        let repr = repr_value.as_object().unwrap();
        let result = resolver.find_entities(repr);
        assert_eq!(result.unwrap().get("name").unwrap(), "Gadget");

        // no match for a product that doesn't exist
        let repr_value = json!({"__typename": "Product", "id": "3"});
        let repr = repr_value.as_object().unwrap();
        let result = resolver.find_entities(repr);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_entities_with_extra_fields_in_storage() {
        // entity has more fields than representation, but they're non-identifying
        let entities = vec![
            json!({
                "__typename": "Status",
                "id": "1",
                "items": [{"id": "i1", "name": "Item"}],
                "statusDetails": "available",
                "internalData": "secret"
            })
            .as_object()
            .unwrap()
            .clone(),
        ];

        let resolver = RootResolver {
            root_mocks: &JsonMap::new(),
            entities: &entities,
        };

        // representation has only the identifying, key fields
        let repr_value = json!({
            "__typename": "Status",
            "id": "1",
            "items": [{"id": "i1", "name": "Item"}]
        });
        let repr = repr_value.as_object().unwrap();
        let result = resolver.find_entities(repr);
        assert_eq!(result.unwrap().get("statusDetails").unwrap(), "available");
    }

    #[test]
    fn test_find_entities_complex_key_with_arrays() {
        // entity with extra fields in its `items` array--non-identifying fields
        let entities = vec![
            json!({
                "__typename": "Stock",
                "id": "1",
                "items": [
                    {"id": "i1", "name": "Item1", "price": 10.99},
                    {"id": "i2", "name": "Item2", "price": 20.99}
                ],
                "stockDetails": "in stock"
            })
            .as_object()
            .unwrap()
            .clone(),
        ];

        let resolver = RootResolver {
            root_mocks: &JsonMap::new(),
            entities: &entities,
        };

        // representation without extra fields in array items
        let repr_value = json!({
            "__typename": "Stock",
            "id": "1",
            "items": [
                {"id": "i1", "name": "Item1"},
                {"id": "i2", "name": "Item2"}
            ]
        });
        let repr = repr_value.as_object().unwrap();
        let result = resolver.find_entities(repr);

        assert_eq!(result.unwrap().get("stockDetails").unwrap(), "in stock");
    }
}
