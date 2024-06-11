use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::NodeStr;
use apollo_federation::sources::source;
use indexmap::IndexSet;
use router_bridge::planner::PlanSuccess;
use router_bridge::planner::Planner;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use tokio::sync::broadcast;
use tower::ServiceExt;
use tracing::instrument;
use tracing::Instrument;

use super::execution::ExecutionParameters;
use super::rewrites;
use super::rewrites::DataRewrite;
use super::selection::execute_selection_set;
use super::selection::Selection;
use super::subgraph_context::ContextualArguments;
use super::subgraph_context::SubgraphContext;
use super::PlanNode;
use super::QueryPlanResult;
use crate::error::Error;
use crate::error::FetchError;
use crate::error::QueryPlannerError;
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
use crate::plugins::connectors::finder_field_for_fetch_node;
use crate::plugins::connectors::Connector;
use crate::services::subgraph::BoxService;
use crate::services::SubgraphRequest;
use crate::spec::query::change::QueryHashVisitor;
use crate::spec::Schema;

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

    // Optionally describes a number of "rewrites" to apply to the data that has already been received further up the tree
    pub(crate) context_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // hash for the query and relevant parts of the schema. if two different schemas provide the exact same types, fields and directives
    // affecting the query, then they will have the same hash
    #[serde(default)]
    pub(crate) schema_aware_hash: Arc<QueryHash>,

    // authorization metadata for the subgraph query
    #[serde(default)]
    pub(crate) authorization: Arc<CacheKeyMetadata>,
    #[serde(default)]
    pub(crate) protocol: Arc<Protocol>,
    #[serde(default, skip)]
    pub(crate) source_node: Option<Arc<source::query_plan::FetchNode>>,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Protocol {
    #[default]
    GraphQL,
    RestWrapper(RestProtocolWrapper),
    RestFetch(RestFetchNode),
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestProtocolWrapper {
    pub(crate) connector_service_name: String,
    pub(crate) connector_graph_key: Option<Arc<String>>,
    pub(crate) magic_finder_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct RestFetchNode {
    pub(crate) connector_service_name: String,
    pub(crate) connector_graph_key: Arc<String>,
    pub(crate) parent_service_name: String,
}

#[derive(Clone)]
pub(crate) struct SubgraphOperation {
    serialized: String,
    /// Ideally this would be always present, but we don’t have access to the subgraph schemas
    /// during `Deserialize`.
    parsed: Option<Arc<Valid<ExecutableDocument>>>,
}

impl SubgraphOperation {
    pub(crate) fn replace(&self, from: &str, to: &str) -> Self {
        let serialized = self.serialized.replace(from, to);

        Self::from_string(serialized)
    }

    pub(crate) fn from_string(serialized: impl Into<String>) -> Self {
        Self {
            serialized: serialized.into(),
            parsed: None,
        }
    }

    pub(crate) fn from_parsed(parsed: impl Into<Arc<Valid<ExecutableDocument>>>) -> Self {
        let parsed = parsed.into();
        Self {
            serialized: parsed.to_string(),
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
        vec![graphql::Error::builder()
            .extension_code(self.code())
            .message(self.to_string())
            .build()]
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

#[derive(Default, Clone)]
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
        variable_usages: &[NodeStr],
        data: &Value,
        current_dir: &Path,
        body: &Request,
        schema: &Schema,
        input_rewrites: &Option<Vec<rewrites::DataRewrite>>,
        context_rewrites: &Option<Vec<rewrites::DataRewrite>>,
    ) -> Option<Variables> {
        let mut subgraph_context = SubgraphContext::new(data, schema, context_rewrites);
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
                            .get_key_value(key.as_str())
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
        service: BoxService,
        subgraph_request: SubgraphRequest,
        service_name: &str,
        current_dir: &Path,
        requires: &[Selection],
        output_rewrites: &Option<Vec<DataRewrite>>,
        schema: &Schema,
        paths: Vec<Vec<Path>>,
        id: Option<NodeStr>,
        deferred_fetches: &HashMap<NodeStr, broadcast::Sender<(Value, Vec<Error>)>>,
        operation_str: &str,
        variables: Map<ByteString, Value>,
    ) -> (Value, Vec<Error>) {
        let (_parts, response) = match service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subfetch_stream"))
            .await
            // TODO this is a problem since it restores details about failed service
            // when errors have been redacted in the include_subgraph_errors module.
            // Unfortunately, not easy to fix here, because at this point we don't
            // know if we should be redacting errors for this subgraph...
            .map_err(|e| match e.downcast::<FetchError>() {
                Ok(inner) => match *inner {
                    FetchError::SubrequestHttpError { .. } => *inner,
                    _ => FetchError::SubrequestHttpError {
                        status_code: None,
                        service: service_name.to_string(),
                        reason: inner.to_string(),
                    },
                },
                Err(e) => FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.to_string(),
                    reason: e.to_string(),
                },
            }) {
            Err(e) => {
                return (
                    Value::default(),
                    vec![e.to_graphql_error(Some(current_dir.to_owned()))],
                );
            }
            Ok(res) => res.response.into_parts(),
        };

        super::log::trace_subfetch(service_name, operation_str, &variables, &response);

        if !response.is_primary() {
            return (
                Value::default(),
                vec![FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_string(),
                }
                .to_graphql_error(Some(current_dir.to_owned()))],
            );
        }

        let (value, errors) = Self::response_at_path(
            schema,
            current_dir,
            paths,
            response,
            requires,
            output_rewrites,
            service_name,
        );
        if let Some(id) = id {
            if let Some(sender) = deferred_fetches.get(id.as_str()) {
                tracing::info!(monotonic_counter.apollo.router.operations.defer.fetch = 1u64);
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, id, e);
                }
            }
        }
        (value, errors)
    }

    #[instrument(skip_all, level = "debug", name = "response_insert")]
    pub(crate) fn response_at_path<'a>(
        schema: &Schema,
        current_dir: &'a Path,
        inverted_paths: Vec<Vec<Path>>,
        response: graphql::Response,
        requires: &[Selection],
        output_rewrites: &Option<Vec<DataRewrite>>,
        service_name: &str,
    ) -> (Value, Vec<Error>) {
        if !requires.is_empty() {
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
                            rewrites::apply_rewrites(schema, &mut entity, output_rewrites);

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
                    service_name
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
                    let path = error.path.as_ref().map(|path| {
                        Path::from_iter(current_slice.iter().chain(path.iter()).cloned())
                    });

                    Error {
                        locations: error.locations,
                        path,
                        message: error.message,
                        extensions: error.extensions,
                    }
                })
                .collect();
            let mut data = response.data.unwrap_or_default();
            rewrites::apply_rewrites(schema, &mut data, output_rewrites);
            (Value::from_path(current_dir, data), errors)
        }
    }

    pub(crate) fn operation_kind(&self) -> &OperationKind {
        &self.operation_kind
    }

    pub(crate) fn init_parsed_operation(
        &mut self,
        subgraph_schemas: &SubgraphSchemas,
    ) -> Result<(), ValidationErrors> {
        let schema = &subgraph_schemas[self.service_name.as_str()];
        self.operation.init_parsed(schema)?;
        Ok(())
    }

    pub(crate) fn init_parsed_operation_and_hash_subquery(
        &mut self,
        subgraph_schemas: &SubgraphSchemas,
        supergraph_schema_hash: &str,
    ) -> Result<(), ValidationErrors> {
        let schema = &subgraph_schemas[self.service_name().as_str()];
        let doc = self.operation.init_parsed(schema)?;

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

    pub(crate) async fn connector_execution<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        current_dir: &'a Path,
        data: &'a Value,
        sender: tokio::sync::mpsc::Sender<graphql::Response>,
        connector_node: &'a PlanNode,
    ) -> Result<(Value, Vec<Error>), FetchError> {
        let Variables {
            variables,
            inverted_paths: paths,
            .. // TODO: context_rewrites
        } = match Variables::new(
            &self.requires,
            self.variable_usages.as_ref(),
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request.body(),
            parameters.schema,
            &self.input_rewrites,
            &self.context_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return Ok((Value::Object(Object::default()), Vec::new()));
            }
        };

        let mut request = parameters.supergraph_request.body().clone();
        request.variables = variables;
        let mut supergraph_request = http::Request::builder()
            .method(parameters.supergraph_request.method())
            .uri(parameters.supergraph_request.uri())
            .body(request)
            .unwrap();
        for (name, value) in parameters.supergraph_request.headers() {
            supergraph_request
                .headers_mut()
                .insert(name.clone(), value.clone());
        }

        let subparameters = ExecutionParameters {
            context: parameters.context,
            service_factory: parameters.service_factory,
            schema: parameters.schema,
            deferred_fetches: parameters.deferred_fetches,
            query: parameters.query,
            root_node: parameters.root_node,
            subscription_handle: parameters.subscription_handle,
            subscription_config: parameters.subscription_config,
            supergraph_request: &Arc::new(supergraph_request),
            connectors: parameters.connectors,
            subgraph_schemas: parameters.subgraph_schemas,
        };

        let path = Path::default();
        let (mut value, errors) = connector_node
            .execute_recursively(&subparameters, &path, data, sender)
            .instrument(tracing::info_span!(
                "connector",
                "graphql.path" = %current_dir,
                "apollo.subgraph.name" = self.service_name.as_str(),
                "otel.kind" = "INTERNAL"
            ))
            .await;

        let magic_finder = match self.protocol.as_ref() {
            Protocol::RestWrapper(wrapper) => wrapper.magic_finder_field.as_ref(),
            _ => None,
        };

        if let Some(magic_finder) = magic_finder {
            let magic_finder = serde_json_bytes::ByteString::from(magic_finder.as_str());
            if let Value::Object(ref mut obj) = value {
                if let Some(v) = obj.remove(&magic_finder) {
                    obj.insert("_entities", v);
                }
            }
        }

        let response = graphql::Response::builder()
            .data(value)
            .errors(errors)
            .build();

        let (value, errors) = Self::response_at_path(
            parameters.schema,
            current_dir,
            paths,
            response,
            &self.requires,
            &self.output_rewrites,
            &self.service_name(),
        );
        if let Some(id) = &self.id {
            if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
                tracing::info!(monotonic_counter.apollo.router.operations.defer.fetch = 1u64);
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                }
            }
        }

        Ok((value, errors))
    }

    pub(crate) async fn generate_connector_plan(
        &mut self,
        schema: &Schema,
        subgraph_planners: &HashMap<Arc<String>, Arc<Planner<QueryPlanResult>>>,
        connectors: &Arc<HashMap<Arc<String>, Connector>>,
    ) -> Result<Option<(PlanSuccess<QueryPlanResult>, RestProtocolWrapper)>, QueryPlannerError>
    {
        if let Some(planner) = subgraph_planners.get(&self.service_name.to_string()) {
            tracing::debug!(
                "planning for subgraph '{}' and query '{}'",
                self.service_name,
                self.operation
            );

            let connectors_in_subgraph = connectors
                .iter()
                .filter_map(|(_, connector)| {
                    if *connector.origin_subgraph == self.service_name.as_str() {
                        Some(connector)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();

            let (operation, rest_protocol_wrapper) = if let Some(rest_protocol_wrapper) =
                finder_field_for_fetch_node(
                    schema,
                    &connectors_in_subgraph,
                    self.requires.as_slice(),
                ) {
                if let Some(mff) = &rest_protocol_wrapper.magic_finder_field {
                    (
                        self.operation.replace("_entities", mff),
                        rest_protocol_wrapper,
                    )
                } else {
                    (self.operation.clone(), rest_protocol_wrapper)
                }
            } else {
                (
                    self.operation.clone(),
                    RestProtocolWrapper {
                        connector_service_name: self.service_name.to_string(),
                        connector_graph_key: None,
                        magic_finder_field: None,
                    },
                )
            };

            tracing::debug!(
                "replaced with operation(magic finder field={:?}): {operation}",
                rest_protocol_wrapper.magic_finder_field.as_ref()
            );
            match planner
                .plan(
                    operation.to_string(),
                    self.operation_name.as_ref().map(|on| on.to_string()),
                    Default::default(),
                )
                .await
                .map_err(QueryPlannerError::RouterBridgeError)?
                .into_result()
            {
                Ok(mut plan) => {
                    if let Some(node) = plan.data.query_plan.node.as_mut() {
                        Arc::make_mut(node)
                            .update_connector_plan(&self.service_name.to_string(), connectors);
                    }

                    return Ok(Some((plan, rest_protocol_wrapper)));
                }
                Err(err) => {
                    return Err(QueryPlannerError::from(err));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn service_name(&self) -> NodeStr {
        match self.protocol.as_ref() {
            Protocol::GraphQL => self.service_name.clone(),
            Protocol::RestWrapper(_rw) => self.service_name.clone(),
            Protocol::RestFetch(rf) => rf.connector_graph_key.to_string().into(),
        }
    }
}
