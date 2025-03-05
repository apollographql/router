use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast;
use apollo_compiler::collections::HashMap;
use apollo_compiler::validation::Valid;
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
use super::selection::Selection;
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
    pub(crate) requires: Vec<Selection>,

    /// The variables that are used for the subgraph fetch.
    pub(crate) variable_usages: Vec<Arc<str>>,

    /// The GraphQL subquery that is used for the fetch.
    pub(crate) operation: SubgraphOperation,

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

#[derive(Clone)]
pub(crate) struct SubgraphOperation {
    serialized: String,
    /// Ideally this would be always present, but we don’t have access to the subgraph schemas
    /// during `Deserialize`.
    parsed: Option<Arc<Valid<ExecutableDocument>>>,
}

impl SubgraphOperation {
    pub(crate) fn from_string(serialized: impl Into<String>) -> Self {
        Self {
            serialized: serialized.into(),
            parsed: None,
        }
    }

    pub(crate) fn from_parsed(parsed: impl Into<Arc<Valid<ExecutableDocument>>>) -> Self {
        let parsed = parsed.into();
        Self {
            serialized: parsed.serialize().no_indent().to_string(),
            parsed: Some(parsed),
        }
    }

    pub(crate) fn as_serialized(&self) -> &str {
        &self.serialized
    }

    pub(crate) fn init_parsed(
        &mut self,
        subgraph_schema: &Valid<apollo_compiler::Schema>,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, ValidationErrors> {
        match &mut self.parsed {
            Some(parsed) => Ok(parsed),
            option => {
                let parsed = Arc::new(ExecutableDocument::parse_and_validate(
                    subgraph_schema,
                    &self.serialized,
                    "operation.graphql",
                )?);
                Ok(option.insert(parsed))
            }
        }
    }

    pub(crate) fn as_parsed(
        &self,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, SubgraphOperationNotInitialized> {
        self.parsed.as_ref().ok_or(SubgraphOperationNotInitialized)
    }
}

/// Failed to call `SubgraphOperation::init_parsed` after creating a query plan
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub(crate) struct SubgraphOperationNotInitialized;

impl SubgraphOperationNotInitialized {
    pub(crate) fn into_graphql_errors(self) -> Vec<Error> {
        vec![
            graphql::Error::builder()
                .extension_code(self.code())
                .message(self.to_string())
                .build(),
        ]
    }

    pub(crate) fn code(&self) -> &'static str {
        "SUBGRAPH_OPERATION_NOT_INITIALIZED"
    }
}

impl Serialize for SubgraphOperation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_serialized().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SubgraphOperation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self::from_string(String::deserialize(deserializer)?))
    }
}

impl PartialEq for SubgraphOperation {
    fn eq(&self, other: &Self) -> bool {
        self.as_serialized() == other.as_serialized()
    }
}

impl std::fmt::Debug for SubgraphOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_serialized(), f)
    }
}

impl std::fmt::Display for SubgraphOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.as_serialized(), f)
    }
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
        requires: &[Selection],
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

        let (value, errors) = self.response_at_path(schema, current_dir, paths, response);

        (value, errors)
    }

    pub(crate) fn deferred_fetches(
        current_dir: &Path,
        id: &Option<String>,
        deferred_fetches: &std::collections::HashMap<String, Sender<(Value, Vec<Error>)>>,
        value: &Value,
        errors: &[Error],
    ) {
        if let Some(id) = id {
            if let Some(sender) = deferred_fetches.get(id.as_str()) {
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
    }

    #[instrument(skip_all, level = "debug", name = "response_insert")]
    pub(crate) fn response_at_path<'a>(
        &'a self,
        schema: &Schema,
        current_dir: &'a Path,
        inverted_paths: Vec<Vec<Path>>,
        response: graphql::Response,
    ) -> (Value, Vec<Error>) {
        if !self.requires.is_empty() {
            let entities_path = Path(vec![json_ext::PathElement::Key(
                "_entities".to_string(),
                None,
            )]);

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
                                    errors.push(Error {
                                        locations: error.locations.clone(),
                                        // append to the entitiy's path the error's path without
                                        //`_entities` and the index
                                        path: Some(Path::from_iter(
                                            values_path.0.iter().chain(&path.0[2..]).cloned(),
                                        )),
                                        message: error.message.clone(),
                                        extensions: error.extensions.clone(),
                                    })
                                }
                            }
                            _ => {
                                error.path = Some(current_dir.clone());
                                errors.push(error)
                            }
                        }
                    } else {
                        error.path = Some(current_dir.clone());
                        errors.push(error);
                    }
                } else {
                    error.path = Some(current_dir.clone());
                    errors.push(error);
                }
            }

            // we have to nest conditions and do early returns here
            // because we need to take ownership of the inner value
            if let Some(Value::Object(mut map)) = response.data {
                if let Some(entities) = map.remove("_entities") {
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

                    Error {
                        locations: error.locations,
                        path: Some(path),
                        message: error.message,
                        extensions: error.extensions,
                    }
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
