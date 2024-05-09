use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::NodeStr;
use indexmap::IndexSet;
use once_cell::sync::OnceCell as OnceLock;
use serde::Deserialize;
use serde::Serialize;
use tower::ServiceExt;
use tracing::instrument;
use tracing::Instrument;

use super::execution::ExecutionParameters;
use super::rewrites;
use super::selection::execute_selection_set;
use super::selection::Selection;
use crate::error::Error;
use crate::error::ValidationErrors;
use crate::graphql::Request;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::FetchRequest;
use crate::spec::query::change::QueryHashVisitor;
use crate::spec::Schema;

/// GraphQL operation type.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
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

impl From<OperationKind> for apollo_compiler::ast::OperationType {
    fn from(value: OperationKind) -> Self {
        match value {
            OperationKind::Query => apollo_compiler::ast::OperationType::Query,
            OperationKind::Mutation => apollo_compiler::ast::OperationType::Mutation,
            OperationKind::Subscription => apollo_compiler::ast::OperationType::Subscription,
        }
    }
}

impl From<apollo_compiler::ast::OperationType> for OperationKind {
    fn from(value: apollo_compiler::ast::OperationType) -> Self {
        match value {
            apollo_compiler::ast::OperationType::Query => OperationKind::Query,
            apollo_compiler::ast::OperationType::Mutation => OperationKind::Mutation,
            apollo_compiler::ast::OperationType::Subscription => OperationKind::Subscription,
        }
    }
}

pub(crate) type SubgraphSchemas = HashMap<String, Arc<Valid<apollo_compiler::Schema>>>;

/// A fetch node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    pub(crate) service_name: NodeStr,

    /// The data that is required for the subgraph fetch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub(crate) requires: Vec<Selection>,

    /// The variables that are used for the subgraph fetch.
    pub(crate) variable_usages: Vec<NodeStr>,

    /// The GraphQL subquery that is used for the fetch.
    pub(crate) operation: SubgraphOperation,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<NodeStr>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    /// Optional id used by Deferred nodes
    pub(crate) id: Option<NodeStr>,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this fetch.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a fetch (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // hash for the query and relevant parts of the schema. if two different schemas provide the exact same types, fields and directives
    // affecting the query, then they will have the same hash
    #[serde(default)]
    pub(crate) schema_aware_hash: Arc<QueryHash>,

    // authorization metadata for the subgraph query
    #[serde(default)]
    pub(crate) authorization: Arc<CacheKeyMetadata>,

    pub(crate) source_id: sources::SourceId,
}

pub(crate) mod sources {
    use apollo_compiler::NodeStr;
    use serde::Deserialize;
    use serde::Serialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) enum SourceId {
        Graphql(GraphqlId),
        Connect(ConnectoId),
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct GraphqlId {
        pub(crate) service_name: NodeStr,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct ConnectoId {
        pub(crate) subgraph_name: NodeStr,
        pub(crate) directive: DirectivePosition,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct DirectivePosition {
        pub(crate) name: NodeStr,
        pub(crate) index: usize,
    }
}

#[derive(Clone)]
pub(crate) struct SubgraphOperation {
    // At least one of these two must be initialized
    serialized: OnceLock<String>,
    parsed: OnceLock<Arc<Valid<ExecutableDocument>>>,
}

impl SubgraphOperation {
    pub(crate) fn from_string(serialized: impl Into<String>) -> Self {
        Self {
            serialized: OnceLock::from(serialized.into()),
            parsed: OnceLock::new(),
        }
    }

    pub(crate) fn from_parsed(parsed: impl Into<Arc<Valid<ExecutableDocument>>>) -> Self {
        Self {
            serialized: OnceLock::new(),
            parsed: OnceLock::from(parsed.into()),
        }
    }

    pub(crate) fn as_serialized(&self) -> &str {
        self.serialized.get_or_init(|| {
            self.parsed
                .get()
                .expect("SubgraphOperation has neither representation initialized")
                .to_string()
        })
    }

    pub(crate) fn as_parsed(
        &self,
        subgraph_schema: &Valid<apollo_compiler::Schema>,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, ValidationErrors> {
        self.parsed.get_or_try_init(|| {
            let serialized = self
                .serialized
                .get()
                .expect("SubgraphOperation has neither representation initialized");
            Ok(Arc::new(
                ExecutableDocument::parse_and_validate(
                    subgraph_schema,
                    serialized,
                    "operation.graphql",
                )
                .map_err(|e| e.errors)?,
            ))
        })
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

#[derive(Clone, Default, Hash, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct QueryHash(#[serde(with = "hex")] pub(crate) Vec<u8>);

impl std::fmt::Debug for QueryHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("QueryHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl Display for QueryHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

pub(crate) struct Variables {
    pub(crate) variables: Object,
    pub(crate) inverted_paths: Vec<Vec<Path>>,
}

impl Variables {
    #[instrument(skip_all, level = "debug", name = "make_variables")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        requires: &[Selection],
        variable_usages: &[NodeStr],
        data: &Value,
        current_dir: &Path,
        request: &Arc<http::Request<Request>>,
        schema: &Schema,
        input_rewrites: &Option<Vec<rewrites::DataRewrite>>,
    ) -> Option<Variables> {
        let body = request.body();
        if !requires.is_empty() {
            let mut variables = Object::with_capacity(1 + variable_usages.len());

            variables.extend(variable_usages.iter().filter_map(|key| {
                body.variables
                    .get_key_value(key.as_str())
                    .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
            }));

            let mut inverted_paths: Vec<Vec<Path>> = Vec::new();
            let mut values: IndexSet<Value> = IndexSet::new();

            data.select_values_and_paths(schema, current_dir, |path, value| {
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

            variables.insert("representations", representations);
            Some(Variables {
                variables,
                inverted_paths,
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
                            .get_key_value(key.as_str())
                            .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                    })
                    .collect::<Object>(),
                inverted_paths: Vec::new(),
            })
        }
    }
}

impl FetchNode {
    pub(crate) fn parsed_operation(
        &self,
        subgraph_schemas: &SubgraphSchemas,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, ValidationErrors> {
        self.operation
            .as_parsed(&subgraph_schemas[self.service_name.as_str()])
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_node<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        data: &'a Value,
        current_dir: &'a Path,
    ) -> (Value, Vec<Error>) {
        // TODO clonessssss
        let fetch_request = FetchRequest {
            fetch_node: self.clone(),
            context: parameters.context.clone(),
            schema: parameters.schema.clone(),
            supergraph_request: parameters.supergraph_request.clone(),
            current_dir: current_dir.clone(),
            data: data.clone(),
        };

        println!("fetch_request for node {:?}", &self);
        let (value, errors) = match parameters
            .fetch_service_factory
            .create()
            .oneshot(fetch_request)
            .instrument(tracing::info_span!("fetch_node"))
            .await
        {
            Ok(response) => response,
            Err(_err) => {
                return (
                    Value::Null,
                    vec![Error::builder()
                        .message("TODO".to_string())
                        .extension_code("TODO".to_string())
                        .build()],
                )
            }
        };

        if let Some(id) = &self.id {
            if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
                tracing::info!(monotonic_counter.apollo.router.operations.defer.fetch = 1u64);
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                }
            }
        }
        (value, errors)
    }

    #[cfg(test)]
    pub(crate) fn service_name(&self) -> &str {
        &self.service_name
    }

    pub(crate) fn operation_kind(&self) -> &OperationKind {
        &self.operation_kind
    }

    pub(crate) fn hash_subquery(
        &mut self,
        subgraph_schemas: &SubgraphSchemas,
        supergraph_schema_hash: &str,
    ) -> Result<(), ValidationErrors> {
        let doc = self.parsed_operation(subgraph_schemas)?;
        let schema = &subgraph_schemas[self.service_name.as_str()];

        if let Ok(hash) = QueryHashVisitor::hash_query(
            schema,
            supergraph_schema_hash,
            doc,
            self.operation_name.as_deref(),
        ) {
            self.schema_aware_hash = Arc::new(QueryHash(hash));
        }
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
