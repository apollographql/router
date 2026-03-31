use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast;
use apollo_compiler::collections::HashMap;
use apollo_compiler::validation::Valid;
use apollo_federation::query_plan::requires_selection;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use indexmap::IndexSet;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use tokio::sync::broadcast::Sender;
use tower::ServiceExt;
use tracing::Instrument;
use tracing::instrument;

use super::rewrites;
use super::selection::execute_selection_set;
use super::subgraph_context::ContextualArguments;
use super::subgraph_context::SubgraphContext;
use crate::error::Error;
use crate::error::FetchError;
use crate::error::ValidationErrors;
use crate::graphql;
use crate::graphql::Request;
use crate::json_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::SubgraphRequest;
use crate::services::fetch::ErrorMapping;
use crate::services::subgraph::BoxService;
use crate::spec::QueryHash;
use crate::spec::Schema;
use crate::spec::SchemaHash;

/// GraphQL operation type.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum OperationKind {
    #[default]
    Query,
    Mutation,
    Subscription,
}

impl Display for OperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.default_type_name())
    }
}

impl OperationKind {
    pub(crate) const fn default_type_name(&self) -> &'static str {
        match self {
            OperationKind::Query => "Query",
            OperationKind::Mutation => "Mutation",
            OperationKind::Subscription => "Subscription",
        }
    }

    /// Only for apollo studio exporter
    pub(crate) const fn as_apollo_operation_type(&self) -> &'static str {
        match self {
            OperationKind::Query => "query",
            OperationKind::Mutation => "mutation",
            OperationKind::Subscription => "subscription",
        }
    }
}

impl From<OperationKind> for ast::OperationType {
    fn from(value: OperationKind) -> Self {
        match value {
            OperationKind::Query => ast::OperationType::Query,
            OperationKind::Mutation => ast::OperationType::Mutation,
            OperationKind::Subscription => ast::OperationType::Subscription,
        }
    }
}

impl From<ast::OperationType> for OperationKind {
    fn from(value: ast::OperationType) -> Self {
        match value {
            ast::OperationType::Query => OperationKind::Query,
            ast::OperationType::Mutation => OperationKind::Mutation,
            ast::OperationType::Subscription => OperationKind::Subscription,
        }
    }
}

pub(crate) type SubgraphSchemas = HashMap<String, SubgraphSchema>;

pub(crate) struct SubgraphSchema {
    pub(crate) schema: Arc<Valid<apollo_compiler::Schema>>,
    // TODO: Ideally should have separate nominal type for subgraph's schema hash
    pub(crate) hash: SchemaHash,
}

impl SubgraphSchema {
    pub(crate) fn new(schema: Valid<apollo_compiler::Schema>) -> Self {
        let sdl = schema.serialize().no_indent().to_string();
        Self {
            schema: Arc::new(schema),
            hash: SchemaHash::new(&sdl),
        }
    }
}

/// A fetch node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    pub(crate) service_name: Arc<str>,

    /// The data that is required for the subgraph fetch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub(crate) requires: Vec<requires_selection::Selection>,

    /// The variables that are used for the subgraph fetch.
    pub(crate) variable_usages: Vec<Arc<str>>,

    /// The GraphQL subquery that is used for the fetch.
    pub(crate) operation: SerializableDocument,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<Arc<str>>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    /// Optional id used by Deferred nodes
    pub(crate) id: Option<String>,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this fetch.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a fetch (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that has already been received further up the tree
    pub(crate) context_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // hash for the query and relevant parts of the schema. if two different schemas provide the exact same types, fields and directives
    // affecting the query, then they will have the same hash
    #[serde(default)]
    pub(crate) schema_aware_hash: Arc<QueryHash>,

    // authorization metadata for the subgraph query
    #[serde(default)]
    pub(crate) authorization: Arc<CacheKeyMetadata>,
}

#[derive(Default)]
pub(crate) struct Variables {
    pub(crate) variables: Object,
    pub(crate) inverted_paths: Vec<Vec<Path>>,
    pub(crate) contextual_arguments: Option<ContextualArguments>,
}

impl Variables {
    #[instrument(skip_all, level = "debug", name = "make_variables")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        requires: &[requires_selection::Selection],
        variable_usages: &[Arc<str>],
        data: &Value,
        current_dir: &Path,
        request: &Arc<http::Request<Request>>,
        schema: &Schema,
        input_rewrites: &Option<Vec<rewrites::DataRewrite>>,
        context_rewrites: &Option<Vec<rewrites::DataRewrite>>,
    ) -> Option<Variables> {
        let body = request.body();
        let mut subgraph_context = SubgraphContext::new(data, schema, context_rewrites);
        if !requires.is_empty() {
            let mut variables = Object::with_capacity(1 + variable_usages.len());

            variables.extend(variable_usages.iter().filter_map(|key| {
                body.variables
                    .get_key_value(key.as_ref())
                    .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
            }));

            let mut inverted_paths: Vec<Vec<Path>> = Vec::new();
            let mut values: IndexSet<Value> = IndexSet::default();
            data.select_values_and_paths(schema, current_dir, |path, value| {
                // first get contextual values that are required
                if let Some(context) = subgraph_context.as_mut() {
                    context.execute_on_path(path);
                }

                let mut value = execute_selection_set(value, requires, schema, None);
                if value.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    rewrites::apply_rewrites(schema, &mut value, input_rewrites);
                    match values.get_index_of(&value) {
                        Some(index) => {
                            inverted_paths[index].push(path.clone());
                        }
                        None => {
                            inverted_paths.push(vec![path.clone()]);
                            values.insert(value);
                            debug_assert!(inverted_paths.len() == values.len());
                        }
                    }
                }
            });

            if values.is_empty() {
                return None;
            }

            let representations = Value::Array(Vec::from_iter(values));
            let contextual_arguments = match subgraph_context.as_mut() {
                Some(context) => context.add_variables_and_get_args(&mut variables),
                None => None,
            };

            variables.insert("representations", representations);
            Some(Variables {
                variables,
                inverted_paths,
                contextual_arguments,
            })
        } else {
            // with nested operations (Query or Mutation has an operation returning a Query or Mutation),
            // when the first fetch fails, the query plan will still execute up until the second fetch,
            // where `requires` is empty (not a federated fetch), the current dir is not emmpty (child of
            // the previous operation field) and the data is null. In that case, we recognize that we
            // should not perform the next fetch
            if !current_dir.is_empty()
                && data
                    .get_path(schema, current_dir)
                    .map(|value| value.is_null())
                    .unwrap_or(true)
            {
                return None;
            }

            Some(Variables {
                variables: variable_usages
                    .iter()
                    .filter_map(|key| {
                        body.variables
                            .get_key_value(key.as_ref())
                            .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                    })
                    .collect::<Object>(),
                inverted_paths: Vec::new(),
                contextual_arguments: None,
            })
        }
    }
}

impl FetchNode {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn subgraph_fetch(
        &self,
        service: BoxService,
        subgraph_request: SubgraphRequest,
        current_dir: &Path,
        schema: &Schema,
        paths: Vec<Vec<Path>>,
        operation_str: &str,
        variables: Map<ByteString, Value>,
        hoist_orphan_errors: bool,
    ) -> (Value, Vec<Error>) {
        let (_parts, response) = match service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subfetch_stream"))
            .await
            .map_to_graphql_error(self.service_name.to_string(), current_dir)
        {
            Err(e) => {
                return (Value::default(), vec![e]);
            }
            Ok(res) => res.response.into_parts(),
        };

        super::log::trace_subfetch(&self.service_name, operation_str, &variables, &response);

        if !response.is_primary() {
            return (
                Value::default(),
                vec![
                    FetchError::SubrequestUnexpectedPatchResponse {
                        service: self.service_name.to_string(),
                    }
                    .to_graphql_error(Some(current_dir.to_owned())),
                ],
            );
        }

        let (value, errors) =
            self.response_at_path(schema, current_dir, paths, response, hoist_orphan_errors);

        (value, errors)
    }

    pub(crate) fn deferred_fetches(
        current_dir: &Path,
        id: &Option<String>,
        deferred_fetches: &std::collections::HashMap<String, Sender<(Value, Vec<Error>)>>,
        value: &Value,
        errors: &[Error],
    ) {
        if let Some(id) = id
            && let Some(sender) = deferred_fetches.get(id.as_str())
        {
            u64_counter!(
                "apollo.router.operations.defer.fetch",
                "Number of deferred responses fetched from subgraphs",
                1
            );
            if let Err(e) = sender.clone().send((value.clone(), Vec::from(errors))) {
                tracing::error!(
                    "error sending fetch result at path {} and id {:?} for deferred response building: {}",
                    current_dir,
                    id,
                    e
                );
            }
        }
    }

    /// Maps a subgraph's response into what can be merged in the overall supergraph response. It
    /// does this by making sure both the data and errors from a subgraph's response can be plugged
    /// into the right slots for the supergraph response, and it does that by a bit of path
    /// handling and manipulation
    ///
    /// When `hoist_orphan_errors` is true, entity-less errors are assigned to the nearest
    /// non-array ancestor of `current_dir`, preventing error multiplication across array
    /// elements. When false, they are assigned to `current_dir` as-is.
    #[instrument(skip_all, level = "debug", name = "response_insert")]
    pub(crate) fn response_at_path<'a>(
        &'a self,
        schema: &Schema,
        current_dir: &'a Path,
        inverted_paths: Vec<Vec<Path>>,
        response: graphql::Response,
        hoist_orphan_errors: bool,
    ) -> (Value, Vec<Error>) {
        if !self.requires.is_empty() {
            let entities_path = Path(vec![json_ext::PathElement::Key(
                "_entities".to_string(),
                None,
            )]);

            // when hoist_orphan_errors is enabled, the fallback_dir is the immediate parent of
            // the current_dir when the current_dir is wildcarded (ie, @, which is PathElement::Flatten)
            //
            // this prevents error multiplication across array elements
            let error_dir = if hoist_orphan_errors {
                let pos = current_dir
                    .0
                    .iter()
                    .position(|e| matches!(e, json_ext::PathElement::Flatten(_)));
                match pos {
                    Some(i) => Path(current_dir.0[..i].to_vec()),
                    None => current_dir.clone(),
                }
            } else {
                current_dir.clone()
            };

            let mut errors: Vec<Error> = vec![];
            for mut error in response.errors {
                // the locations correspond to the subgraph query and cannot be linked to locations
                // in the client query, so we remove them
                error.locations = Vec::new();

                // errors with path should be updated to the path of the entity they target
                if let Some(ref path) = error.path {
                    if path.starts_with(&entities_path) {
                        // the error's path has the format '/_entities/1/other' so we ignore the
                        // first element and then get the index
                        match path.0.get(1) {
                            Some(json_ext::PathElement::Index(i)) => {
                                for values_path in
                                    inverted_paths.get(*i).iter().flat_map(|v| v.iter())
                                {
                                    errors.push(
                                        Error::builder()
                                            .locations(error.locations.clone())
                                            // append to the entity's path the error's path without
                                            //`_entities` and the index
                                            .path(Path::from_iter(
                                                values_path.0.iter().chain(&path.0[2..]).cloned(),
                                            ))
                                            .message(error.message.clone())
                                            .and_extension_code(error.extension_code())
                                            .extensions(error.extensions.clone())
                                            // re-use the original ID so we don't double count this error
                                            .apollo_id(error.apollo_id())
                                            .build(),
                                    )
                                }
                            }
                            _ => {
                                error.path = Some(error_dir.clone());
                                errors.push(error)
                            }
                        }
                    } else {
                        error.path = Some(error_dir.clone());
                        errors.push(error);
                    }
                } else {
                    error.path = Some(error_dir.clone());
                    errors.push(error);
                }
            }

            // we have to nest conditions and do early returns here
            // because we need to take ownership of the inner value
            if let Some(Value::Object(mut map)) = response.data
                && let Some(entities) = map.remove("_entities")
            {
                tracing::trace!("received entities: {:?}", &entities);

                if let Value::Array(array) = entities {
                    let mut value = Value::default();

                    for (index, mut entity) in array.into_iter().enumerate() {
                        rewrites::apply_rewrites(schema, &mut entity, &self.output_rewrites);

                        if let Some(paths) = inverted_paths.get(index) {
                            if paths.len() > 1 {
                                for path in &paths[1..] {
                                    let _ = value.insert(path, entity.clone());
                                }
                            }

                            if let Some(path) = paths.first() {
                                let _ = value.insert(path, entity);
                            }
                        }
                    }
                    return (value, errors);
                }
            }

            // if we get here, it means that the response was missing the `_entities` key
            // This can happen if the subgraph failed during query execution e.g. for permissions checks.
            // In this case we should add an additional error because the subgraph should have returned an error that will be bubbled up to the client.
            // However, if they have not then print a warning to the logs.
            if errors.is_empty() {
                tracing::warn!(
                    "Subgraph response from '{}' was missing key `_entities` and had no errors. This is likely a bug in the subgraph.",
                    self.service_name
                );
            }

            (Value::Null, errors)
        } else {
            let current_slice =
                if matches!(current_dir.last(), Some(&json_ext::PathElement::Flatten(_))) {
                    &current_dir.0[..current_dir.0.len() - 1]
                } else {
                    &current_dir.0[..]
                };

            let errors: Vec<Error> = response
                .errors
                .into_iter()
                .map(|error| {
                    let path = error
                        .path
                        .as_ref()
                        .map(|path| {
                            Path::from_iter(current_slice.iter().chain(path.iter()).cloned())
                        })
                        .unwrap_or_else(|| current_dir.clone());

                    Error::builder()
                        .locations(error.locations.clone())
                        .path(path)
                        .message(error.message.clone())
                        .and_extension_code(error.extension_code())
                        .extensions(error.extensions.clone())
                        .apollo_id(error.apollo_id())
                        .build()
                })
                .collect();
            let mut data = response.data.unwrap_or_default();
            rewrites::apply_rewrites(schema, &mut data, &self.output_rewrites);
            (Value::from_path(current_dir, data), errors)
        }
    }

    #[cfg(test)]
    pub(crate) fn service_name(&self) -> &str {
        &self.service_name
    }

    pub(crate) fn operation_kind(&self) -> &OperationKind {
        &self.operation_kind
    }

    pub(crate) fn init_parsed_operation(
        &mut self,
        subgraph_schemas: &SubgraphSchemas,
    ) -> Result<(), ValidationErrors> {
        let schema = &subgraph_schemas[self.service_name.as_ref()];
        self.operation.init_parsed(&schema.schema)?;
        Ok(())
    }

    pub(crate) fn init_parsed_operation_and_hash_subquery(
        &mut self,
        subgraph_schemas: &SubgraphSchemas,
    ) -> Result<(), ValidationErrors> {
        let schema = &subgraph_schemas[self.service_name.as_ref()];
        self.operation.init_parsed(&schema.schema)?;
        self.schema_aware_hash = Arc::new(schema.hash.operation_hash(
            self.operation.as_serialized(),
            self.operation_name.as_deref(),
        ));
        Ok(())
    }

    pub(crate) fn extract_authorization_metadata(
        &mut self,
        schema: &Valid<apollo_compiler::Schema>,
        global_authorisation_cache_key: &CacheKeyMetadata,
    ) {
        let doc = ExecutableDocument::parse(
            schema,
            self.operation.as_serialized().to_string(),
            "query.graphql",
        )
        // Assume query planing creates a valid document: ignore parse errors
        .unwrap_or_else(|invalid| invalid.partial);
        let subgraph_query_cache_key = AuthorizationPlugin::generate_cache_metadata(
            &doc,
            self.operation_name.as_deref(),
            schema,
            !self.requires.is_empty(),
        );

        // we need to intersect the cache keys because the global key already takes into account
        // the scopes and policies from the client request
        self.authorization = Arc::new(AuthorizationPlugin::intersect_cache_keys_subgraph(
            global_authorisation_cache_key,
            &subgraph_query_cache_key,
        ));
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_federation::query_plan::requires_selection;
    use apollo_federation::query_plan::serializable_document::SerializableDocument;
    use rstest::rstest;
    use serde_json_bytes::json;

    use super::*;
    use crate::Configuration;

    fn test_schema() -> Schema {
        let sdl = r#"
            schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
                query: Query
            }
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            scalar link__Import
            scalar join__FieldSet

            enum link__Purpose { SECURITY EXECUTION }

            enum join__Graph {
                TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
            }

            type Query {
                me: String
            }
        "#;
        Schema::parse(sdl, &Configuration::default()).unwrap()
    }

    fn make_fetch_node(requires: Vec<requires_selection::Selection>) -> FetchNode {
        FetchNode {
            service_name: "test".into(),
            requires,
            variable_usages: vec![],
            operation: SerializableDocument::from_string("{ me }"),
            operation_name: None,
            operation_kind: OperationKind::Query,
            id: None,
            input_rewrites: None,
            output_rewrites: None,
            context_rewrites: None,
            schema_aware_hash: Default::default(),
            authorization: Default::default(),
        }
    }

    fn make_requires() -> Vec<requires_selection::Selection> {
        vec![requires_selection::Selection::InlineFragment(
            requires_selection::InlineFragment {
                type_condition: Some(name!("T")),
                selections: vec![requires_selection::Selection::Field(
                    requires_selection::Field {
                        alias: None,
                        name: name!("id"),
                        selections: Vec::new(),
                    },
                )],
            },
        )]
    }

    fn key(name: &str) -> json_ext::PathElement {
        json_ext::PathElement::Key(name.to_string(), None)
    }

    fn index(i: usize) -> json_ext::PathElement {
        json_ext::PathElement::Index(i)
    }

    fn flatten() -> json_ext::PathElement {
        json_ext::PathElement::Flatten(None)
    }

    fn make_error(path: Option<Path>) -> graphql::Error {
        match path {
            Some(p) => graphql::Error::builder().message("err").path(p).build(),
            None => graphql::Error::builder().message("err").build(),
        }
    }

    #[rstest]
    #[case::single_key(
        vec![key("topLevel")],
        Some(json!({"field": "value"})),
        json!({"topLevel": {"field": "value"}})
    )]
    #[case::no_data(
        vec![key("topLevel")],
        None,
        json!({"topLevel": null})
    )]
    #[case::empty_current_dir(
        vec![],
        Some(json!({"me": "hello"})),
        json!({"me": "hello"})
    )]
    #[case::deep_nesting(
        vec![key("a"), key("b"), key("c"), key("d")],
        Some(json!({"value": 42})),
        json!({"a": {"b": {"c": {"d": {"value": 42}}}}})
    )]
    fn root_fetch_data_wrapping(
        #[case] dir_elements: Vec<json_ext::PathElement>,
        #[case] data: Option<Value>,
        #[case] expected: Value,
    ) {
        let schema = test_schema();
        let node = make_fetch_node(vec![]);
        let current_dir = Path(dir_elements);
        let response = graphql::Response {
            data,
            ..Default::default()
        };
        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert!(errors.is_empty());
        assert_eq!(value, expected);
    }

    #[rstest]
    #[case::prepends_current_dir(
        vec![key("top"), key("nested")],
        Some(Path(vec![key("field")])),
        Path(vec![key("top"), key("nested"), key("field")])
    )]
    #[case::no_error_path_uses_current_dir(
        vec![key("top")],
        None,
        Path(vec![key("top")])
    )]
    #[case::trailing_flatten_stripped(
        vec![key("list"), flatten()],
        Some(Path(vec![key("name")])),
        Path(vec![key("list"), key("name")])
    )]
    #[case::no_error_path_keeps_flatten(
        vec![key("list"), flatten()],
        None,
        Path(vec![key("list"), flatten()])
    )]
    #[case::index_in_error_path(
        vec![key("items")],
        Some(Path(vec![index(2), key("name")])),
        Path(vec![key("items"), index(2), key("name")])
    )]
    #[case::flatten_mid_path_not_stripped(
        vec![key("a"), flatten(), key("b")],
        Some(Path(vec![key("c")])),
        Path(vec![key("a"), flatten(), key("b"), key("c")])
    )]
    fn root_fetch_error_path(
        #[case] dir_elements: Vec<json_ext::PathElement>,
        #[case] error_path: Option<Path>,
        #[case] expected_path: Path,
    ) {
        let schema = test_schema();
        let node = make_fetch_node(vec![]);
        let current_dir = Path(dir_elements);
        let response = graphql::Response::builder()
            .error(make_error(error_path))
            .build();

        let (_, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.as_ref().unwrap(), &expected_path);
    }

    #[test]
    fn root_fetch_multiple_errors() {
        let schema = test_schema();
        let node = make_fetch_node(vec![]);
        let current_dir = Path(vec![key("root")]);
        let response = graphql::Response::builder()
            .error(
                graphql::Error::builder()
                    .message("error 1")
                    .path(Path(vec![key("a")]))
                    .build(),
            )
            .error(
                graphql::Error::builder()
                    .message("error 2")
                    .path(Path(vec![key("b")]))
                    .build(),
            )
            .error(graphql::Error::builder().message("error 3").build())
            .build();

        let (_, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("root"), key("a")])
        );
        assert_eq!(
            errors[1].path.as_ref().unwrap(),
            &Path(vec![key("root"), key("b")])
        );
        assert_eq!(errors[2].path.as_ref().unwrap(), &Path(vec![key("root")]));
    }

    #[test]
    fn root_fetch_preserves_error_extension_code() {
        let schema = test_schema();
        let node = make_fetch_node(vec![]);
        let current_dir = Path(vec![key("root")]);
        let response = graphql::Response::builder()
            .error(
                graphql::Error::builder()
                    .message("auth error")
                    .extension_code("UNAUTHORIZED")
                    .path(Path(vec![key("field")]))
                    .build(),
            )
            .build();

        let (_, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].extension_code().as_deref(), Some("UNAUTHORIZED"));
    }

    #[rstest]
    #[case::entities_path_no_index(
        vec![key("users"), flatten()],
        Some(Path(vec![key("_entities")])),
        Path(vec![key("users")])
    )]
    #[case::non_entities_prefix(
        vec![key("a"), key("b")],
        Some(Path(vec![key("other"), key("field")])),
        Path(vec![key("a"), key("b")])
    )]
    #[case::no_path_truncates_at_flatten(
        vec![key("a"), flatten(), key("b")],
        None,
        Path(vec![key("a")])
    )]
    #[case::no_flatten_equals_current_dir(
        vec![key("a"), key("b"), key("c")],
        None,
        Path(vec![key("a"), key("b"), key("c")])
    )]
    #[case::two_flattens_truncates_at_first(
        vec![key("a"), flatten(), key("b"), flatten()],
        None,
        Path(vec![key("a")])
    )]
    #[case::entities_key_not_index(
        vec![key("root")],
        Some(Path(vec![key("_entities"), key("notAnIndex")])),
        Path(vec![key("root")])
    )]
    fn entity_error_uses_fallback_dir(
        #[case] dir_elements: Vec<json_ext::PathElement>,
        #[case] error_path: Option<Path>,
        #[case] expected_path: Path,
    ) {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(dir_elements);
        let response = graphql::Response::builder()
            .data(json!({"_entities": []}))
            .error(make_error(error_path))
            .build();

        let (_, errors) = node.response_at_path(&schema, &current_dir, vec![], response, true);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.as_ref().unwrap(), &expected_path);
    }

    #[rstest]
    #[case::flatten_preserved_when_disabled(
        vec![key("users"), flatten()],
        None,
        Path(vec![key("users"), flatten()])
    )]
    #[case::nested_flatten_preserved_when_disabled(
        vec![key("a"), flatten(), key("b"), flatten()],
        None,
        Path(vec![key("a"), flatten(), key("b"), flatten()])
    )]
    #[case::non_entities_path_gets_current_dir_when_disabled(
        vec![key("items"), flatten()],
        Some(Path(vec![key("something")])),
        Path(vec![key("items"), flatten()])
    )]
    fn entity_error_uses_current_dir_when_hoist_disabled(
        #[case] dir_elements: Vec<json_ext::PathElement>,
        #[case] error_path: Option<Path>,
        #[case] expected_path: Path,
    ) {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(dir_elements);
        let response = graphql::Response::builder()
            .data(json!({"_entities": []}))
            .error(make_error(error_path))
            .build();

        let (_, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.as_ref().unwrap(), &expected_path);
    }

    #[test]
    fn entity_fetch_basic_entities_inserted_at_inverted_paths() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("topField"), flatten()]);
        let inverted_paths = vec![
            vec![Path(vec![key("topField"), index(0)])],
            vec![Path(vec![key("topField"), index(1)])],
        ];
        let response = graphql::Response::builder()
            .data(json!({
                "_entities": [
                    {"name": "Alice"},
                    {"name": "Bob"}
                ]
            }))
            .build();

        let (value, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert!(errors.is_empty());
        let top = value.as_object().unwrap().get("topField").unwrap();
        let arr = top.as_array().unwrap();
        assert_eq!(arr[0], json!({"name": "Alice"}));
        assert_eq!(arr[1], json!({"name": "Bob"}));
    }

    #[test]
    fn entity_fetch_entity_at_multiple_inverted_paths() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("field"), flatten()]);
        let inverted_paths = vec![vec![
            Path(vec![key("field"), index(0)]),
            Path(vec![key("field"), index(2)]),
        ]];
        let response = graphql::Response::builder()
            .data(json!({
                "_entities": [{"name": "Alice"}]
            }))
            .build();

        let (value, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert!(errors.is_empty());
        let arr = value
            .as_object()
            .unwrap()
            .get("field")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(arr[0], json!({"name": "Alice"}));
        assert_eq!(arr[2], json!({"name": "Alice"}));
    }

    #[test]
    fn entity_fetch_empty_entities_array_returns_default_value() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("field")]);
        let response = graphql::Response::builder()
            .data(json!({"_entities": []}))
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert!(errors.is_empty());
        assert_eq!(value, Value::default());
    }

    #[test]
    fn entity_fetch_more_entities_than_inverted_paths() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("f"), flatten()]);
        let inverted_paths = vec![vec![Path(vec![key("f"), index(0)])]];
        let response = graphql::Response::builder()
            .data(json!({
                "_entities": [
                    {"name": "Alice"},
                    {"name": "Bob"},
                    {"name": "Charlie"}
                ]
            }))
            .build();

        let (value, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert!(errors.is_empty());
        let arr = value
            .as_object()
            .unwrap()
            .get("f")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(arr[0], json!({"name": "Alice"}));
    }

    #[test]
    fn entity_fetch_error_with_entities_path_and_index_remapped() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("users"), flatten()]);
        let inverted_paths = vec![
            vec![Path(vec![key("users"), index(0)])],
            vec![Path(vec![key("users"), index(1)])],
        ];
        let response = graphql::Response::builder()
            .data(json!({"_entities": [null, null]}))
            .error(
                graphql::Error::builder()
                    .message("entity error")
                    .path(Path(vec![key("_entities"), index(1), key("name")]))
                    .build(),
            )
            .build();

        let (_, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("users"), index(1), key("name")])
        );
        assert_eq!(errors[0].message, "entity error");
    }

    #[test]
    fn entity_fetch_error_locations_cleared() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("data")]);
        let response = graphql::Response::builder()
            .data(json!({"_entities": [null]}))
            .error(
                graphql::Error::builder()
                    .message("err")
                    .locations(vec![graphql::Location { line: 1, column: 5 }])
                    .path(Path(vec![key("_entities"), index(0), key("x")]))
                    .build(),
            )
            .build();

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            vec![vec![Path(vec![key("data"), index(0)])]],
            response,
            false,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].locations.is_empty());
    }

    #[test]
    fn entity_fetch_error_index_remapped_to_multiple_inverted_paths() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("items"), flatten()]);
        let inverted_paths = vec![vec![
            Path(vec![key("items"), index(0)]),
            Path(vec![key("items"), index(3)]),
        ]];
        let response = graphql::Response::builder()
            .data(json!({"_entities": [null]}))
            .error(
                graphql::Error::builder()
                    .message("err")
                    .path(Path(vec![key("_entities"), index(0), key("name")]))
                    .build(),
            )
            .build();

        let (_, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("items"), index(0), key("name")])
        );
        assert_eq!(
            errors[1].path.as_ref().unwrap(),
            &Path(vec![key("items"), index(3), key("name")])
        );
    }

    #[test]
    fn entity_fetch_error_index_out_of_bounds_inverted_paths_no_panic() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("x")]);
        let response = graphql::Response::builder()
            .data(json!({"_entities": []}))
            .error(
                graphql::Error::builder()
                    .message("oob")
                    .path(Path(vec![key("_entities"), index(5), key("f")]))
                    .build(),
            )
            .build();

        let (_, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert!(errors.is_empty());
    }

    #[test]
    fn entity_fetch_preserves_extension_code_on_remapped_errors() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("users"), flatten()]);
        let inverted_paths = vec![vec![Path(vec![key("users"), index(0)])]];
        let response = graphql::Response::builder()
            .data(json!({"_entities": [null]}))
            .error(
                graphql::Error::builder()
                    .message("forbidden")
                    .extension_code("FORBIDDEN")
                    .path(Path(vec![key("_entities"), index(0)]))
                    .build(),
            )
            .build();

        let (_, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].extension_code().as_deref(), Some("FORBIDDEN"));
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("users"), index(0)])
        );
    }

    #[test]
    fn entity_fetch_error_appends_remaining_path_after_index() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("data"), flatten()]);
        let inverted_paths = vec![vec![Path(vec![key("data"), index(0)])]];
        let response = graphql::Response::builder()
            .data(json!({"_entities": [null]}))
            .error(
                graphql::Error::builder()
                    .message("nested err")
                    .path(Path(vec![
                        key("_entities"),
                        index(0),
                        key("address"),
                        key("city"),
                    ]))
                    .build(),
            )
            .build();

        let (_, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, false);

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("data"), index(0), key("address"), key("city")])
        );
    }

    #[test]
    fn entity_fetch_missing_entities_key_with_errors() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("users"), flatten()]);
        let response = graphql::Response::builder()
            .data(json!({"something": "else"}))
            .error(
                graphql::Error::builder()
                    .message("permission denied")
                    .build(),
            )
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(value, Value::Null);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "permission denied");
    }

    #[test]
    fn entity_fetch_missing_entities_key_no_errors() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("users")]);
        let response = graphql::Response::builder()
            .data(json!({"something": "else"}))
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(value, Value::Null);
        assert!(errors.is_empty());
    }

    #[test]
    fn entity_fetch_null_data_returns_null_with_errors() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("field")]);
        let response = graphql::Response::builder()
            .error(graphql::Error::builder().message("subgraph error").build())
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(value, Value::Null);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn entity_fetch_null_data_errors_get_fallback_dir() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("users"), flatten(), key("reviews")]);
        let expected_fallback = Path(vec![key("users")]);
        let response = graphql::Response::builder()
            .error(graphql::Error::builder().message("pathless error").build())
            .error(
                graphql::Error::builder()
                    .message("non-entities path")
                    .path(Path(vec![key("something")]))
                    .build(),
            )
            .error(
                graphql::Error::builder()
                    .message("entities no index")
                    .path(Path(vec![key("_entities")]))
                    .build(),
            )
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, true);

        assert_eq!(value, Value::Null);
        assert_eq!(errors.len(), 3);
        for error in &errors {
            assert_eq!(
                error.path.as_ref().unwrap(),
                &expected_fallback,
                "error '{}' did not get fallback_dir",
                error.message,
            );
        }
    }

    #[test]
    fn entity_fetch_missing_entities_key_errors_get_fallback_dir() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("items"), flatten()]);
        let expected_fallback = Path(vec![key("items")]);
        let response = graphql::Response::builder()
            .data(json!({"something": "else"}))
            .error(
                graphql::Error::builder()
                    .message("permission denied")
                    .build(),
            )
            .error(
                graphql::Error::builder()
                    .message("other error")
                    .path(Path(vec![key("unrelated")]))
                    .build(),
            )
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, true);

        assert_eq!(value, Value::Null);
        assert_eq!(errors.len(), 2);
        for error in &errors {
            assert_eq!(
                error.path.as_ref().unwrap(),
                &expected_fallback,
                "error '{}' did not get fallback_dir",
                error.message,
            );
        }
    }

    #[test]
    fn entity_fetch_entities_not_array_returns_null() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("field")]);
        let response = graphql::Response::builder()
            .data(json!({"_entities": "not_an_array"}))
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, false);

        assert_eq!(value, Value::Null);
        assert!(errors.is_empty());
    }

    #[test]
    fn entity_fetch_entities_not_array_errors_get_fallback_dir() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("products"), flatten()]);
        let expected_fallback = Path(vec![key("products")]);
        let response = graphql::Response::builder()
            .data(json!({"_entities": 42}))
            .error(graphql::Error::builder().message("bad entities").build())
            .build();

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, true);

        assert_eq!(value, Value::Null);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.as_ref().unwrap(), &expected_fallback);
    }

    #[test]
    fn entity_fetch_data_is_non_object_returns_null_with_fallback_errors() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("orders"), flatten(), key("items")]);
        let expected_fallback = Path(vec![key("orders")]);
        let response = graphql::Response {
            data: Some(Value::Null),
            errors: vec![graphql::Error::builder().message("null data error").build()],
            ..Default::default()
        };

        let (value, errors) = node.response_at_path(&schema, &current_dir, vec![], response, true);

        assert_eq!(value, Value::Null);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.as_ref().unwrap(), &expected_fallback);
    }

    #[test]
    fn entity_fetch_mixed_error_types() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("users"), flatten()]);
        let inverted_paths = vec![
            vec![Path(vec![key("users"), index(0)])],
            vec![Path(vec![key("users"), index(1)])],
        ];
        let response = graphql::Response::builder()
            .data(json!({"_entities": [{"name": "Alice"}, null]}))
            .error(
                graphql::Error::builder()
                    .message("entity 1 error")
                    .path(Path(vec![key("_entities"), index(1), key("field")]))
                    .build(),
            )
            .error(
                graphql::Error::builder()
                    .message("general error")
                    .path(Path(vec![key("other")]))
                    .build(),
            )
            .error(graphql::Error::builder().message("pathless").build())
            .build();

        let (_, errors) =
            node.response_at_path(&schema, &current_dir, inverted_paths, response, true);

        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("users"), index(1), key("field")])
        );
        assert_eq!(errors[1].path.as_ref().unwrap(), &Path(vec![key("users")]));
        assert_eq!(errors[2].path.as_ref().unwrap(), &Path(vec![key("users")]));
    }
}
