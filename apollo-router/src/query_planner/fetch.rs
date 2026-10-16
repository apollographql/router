use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use apollo_federation::query_plan::requires_selection;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use apollo_json::JsonKind;
use apollo_json::PendingTree;
use apollo_json::ValueBuilder;
use indexmap::IndexSet;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::broadcast::Sender;
use tower::ServiceExt;
use tracing::Instrument;
use tracing::instrument;

use super::log::SubfetchTrace;
use super::rewrites;
use super::selection::select;
use super::subgraph_context::ContextualArguments;
use super::subgraph_context::SubgraphContext;
use crate::error::Error;
use crate::error::FetchError;
use crate::error::ValidationErrors;
use crate::graphql;
use crate::graphql::Request;
use crate::json_ext;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::query_planner::HashedSubgraphSchemas;
use crate::query_planner::SubgraphSchemas;
use crate::services::SubgraphRequest;
use crate::services::fetch::ErrorMapping;
use crate::services::subgraph::BoxCloneService;
use crate::spec::QueryHash;
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

/// The response paths each representation of a fetch came from, grouped by
/// representation in the order the paths were found.
///
/// Every path lives in one buffer of elements, so a fetch of thousands of
/// entities allocates a few buffers rather than a vector per path and per
/// representation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct InvertedPaths {
    elements: Vec<json_ext::PathElement>,
    /// The end of each path in `elements`.
    path_ends: Vec<usize>,
    /// The end of each representation's run of paths in `path_ends`.
    group_ends: Vec<usize>,
}

impl InvertedPaths {
    /// Groups the paths in `elements` (ending at `path_ends`) by
    /// representation: `group_of[p]` is the representation path `p` produced,
    /// and there are `groups` representations. Paths keep their order within
    /// a group.
    fn grouped(
        elements: Vec<json_ext::PathElement>,
        path_ends: Vec<usize>,
        group_of: &[usize],
        groups: usize,
    ) -> Self {
        // Without duplicates, each path is its own group, in order.
        if groups == path_ends.len() {
            return InvertedPaths {
                group_ends: (1..=groups).collect(),
                elements,
                path_ends,
            };
        }
        let start = |p: usize| if p == 0 { 0 } else { path_ends[p - 1] };
        let mut by_group: Vec<Vec<usize>> = vec![Vec::new(); groups];
        for (path, &group) in group_of.iter().enumerate() {
            by_group[group].push(path);
        }
        let mut sorted = InvertedPaths {
            elements: Vec::with_capacity(elements.len()),
            path_ends: Vec::with_capacity(path_ends.len()),
            group_ends: Vec::with_capacity(groups),
        };
        for paths in by_group {
            for path in paths {
                sorted
                    .elements
                    .extend_from_slice(&elements[start(path)..path_ends[path]]);
                sorted.path_ends.push(sorted.elements.len());
            }
            sorted.group_ends.push(sorted.path_ends.len());
        }
        sorted
    }

    /// The path at `index` among all paths.
    fn path(&self, index: usize) -> &[json_ext::PathElement] {
        let start = if index == 0 {
            0
        } else {
            self.path_ends[index - 1]
        };
        &self.elements[start..self.path_ends[index]]
    }

    /// The paths representation `representation` came from, in the order
    /// they were found.
    pub(crate) fn get(
        &self,
        representation: usize,
    ) -> Option<impl ExactSizeIterator<Item = &[json_ext::PathElement]> + '_> {
        let end = *self.group_ends.get(representation)?;
        let start = if representation == 0 {
            0
        } else {
            self.group_ends[representation - 1]
        };
        Some((start..end).map(|path| self.path(path)))
    }

    /// Every path, grouped by representation.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &[json_ext::PathElement]> + '_ {
        (0..self.path_ends.len()).map(|path| self.path(path))
    }
}

impl From<Vec<Vec<Path>>> for InvertedPaths {
    fn from(groups: Vec<Vec<Path>>) -> Self {
        let mut paths = InvertedPaths::default();
        for group in groups {
            for path in group {
                paths.elements.extend(path.0);
                paths.path_ends.push(paths.elements.len());
            }
            paths.group_ends.push(paths.path_ends.len());
        }
        paths
    }
}

#[derive(Default)]
pub(crate) struct Variables {
    pub(crate) variables: Value,
    /// Shared, so the execution and fetch services hold it without copying
    /// every path.
    pub(crate) inverted_paths: Arc<InvertedPaths>,
    pub(crate) contextual_arguments: Option<ContextualArguments>,
}

/// A builder holding the entries of `body_variables` that this fetch declares a usage of.
fn used_variables(variable_usages: &[Arc<str>], body_variables: &Value) -> ValueBuilder {
    let mut builder = ValueBuilder::new();
    for key in variable_usages {
        if let Some(value) = body_variables.get(key.as_ref()) {
            builder
                .set(key.as_ref(), value)
                .expect("the variables builder always has an object root");
        }
    }
    builder
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
            let mut variables = used_variables(variable_usages, &body.variables);

            // Every representation goes into one pending tree and then one
            // arena, written in one pass, so the representations cost a few
            // buffers rather than a vector per object and an arena each.
            let mut tree = PendingTree::new();
            let pending = tree.array();
            let mut elements: Vec<json_ext::PathElement> = Vec::new();
            let mut path_ends: Vec<usize> = Vec::new();
            data.select_values_and_paths(schema, current_dir, |path, value| {
                // first get contextual values that are required
                if let Some(context) = subgraph_context.as_mut() {
                    context.execute_on_path(path);
                }

                // Written as a root and appended only once it turns out to
                // be a non-empty object.
                let representation = tree.root();
                select(&mut tree, representation, &value, requires, schema, None);
                if tree.first(representation).is_some() {
                    tree.append(pending, representation);
                    elements.extend_from_slice(&path.0);
                    path_ends.push(elements.len());
                }
            });

            if path_ends.is_empty() {
                return None;
            }

            let pending = tree.seal(pending);
            let mut values: IndexSet<Value> = IndexSet::with_capacity(path_ends.len());
            let mut group_of: Vec<usize> = Vec::with_capacity(path_ends.len());
            for mut value in pending.array_iter() {
                rewrites::apply_rewrites(schema, &mut value, input_rewrites);
                let group = match values.get_index_of(&value) {
                    Some(index) => index,
                    None => {
                        values.insert(value);
                        values.len() - 1
                    }
                };
                group_of.push(group);
            }
            let inverted_paths =
                InvertedPaths::grouped(elements, path_ends, &group_of, values.len());

            // Without duplicates or rewrites, the pending array is already
            // the representations list.
            let representations = if input_rewrites.is_none() && Some(values.len()) == pending.len()
            {
                pending
            } else {
                Value::array(values)
            };
            let contextual_arguments = match subgraph_context.as_mut() {
                Some(context) => context.add_variables_and_get_args(&mut variables),
                None => None,
            };

            variables
                .set("representations", representations)
                .expect("the variables builder always has an object root");
            Some(Variables {
                variables: variables.seal(),
                inverted_paths: Arc::new(inverted_paths),
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
                    .select_path(schema, current_dir)
                    .map(|value| value.is_null())
                    .unwrap_or(true)
            {
                return None;
            }

            Some(Variables {
                variables: used_variables(variable_usages, &body.variables).seal(),
                inverted_paths: Default::default(),
                contextual_arguments: None,
            })
        }
    }
}

impl FetchNode {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn subgraph_fetch(
        &self,
        service: BoxCloneService,
        subgraph_request: SubgraphRequest,
        current_dir: &Path,
        schema: &Schema,
        paths: Arc<InvertedPaths>,
        subfetch_trace: Option<SubfetchTrace>,
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

        if let Some(subfetch_trace) = &subfetch_trace {
            super::log::trace_subfetch(&self.service_name, subfetch_trace, &response);
        }

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
            self.response_at_path(schema, current_dir, &paths, response, hoist_orphan_errors);

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
        inverted_paths: &InvertedPaths,
        response: graphql::Response,
        hoist_orphan_errors: bool,
    ) -> (Value, Vec<Error>) {
        if !self.requires.is_empty() {
            let entities_path = Path(vec![json_ext::PathElement::Key("_entities".into(), None)]);

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
                                for values_path in inverted_paths.get(*i).into_iter().flatten() {
                                    let mut new_error = Error::builder()
                                        .locations(error.locations.clone())
                                        // append to the entity's path the error's path without
                                        //`_entities` and the index
                                        .path(Path::from_iter(
                                            values_path.iter().chain(&path.0[2..]).cloned(),
                                        ))
                                        .message(error.message.clone())
                                        .and_extension_code(error.extension_code())
                                        .extensions(error.extensions.clone())
                                        // re-use the original ID so we don't double count this error
                                        .apollo_id(error.apollo_id())
                                        .build();
                                    new_error.set_span_event_emitted(error.span_event_emitted());
                                    errors.push(new_error)
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

            if let Some(entities) = response
                .data
                .as_ref()
                .and_then(|data| data.get("_entities"))
            {
                tracing::trace!("received entities: {:?}", &entities);

                if entities.kind() == JsonKind::Array {
                    // One builder places every entity, sealed once at the end.
                    let mut placed = Value::default().edit();

                    for (index, mut entity) in entities.array_iter().enumerate() {
                        rewrites::apply_rewrites(schema, &mut entity, &self.output_rewrites);

                        if let Some(mut paths) = inverted_paths.get(index) {
                            // The first path takes the entity; any others get a
                            // reference to it.
                            let first = paths.next();
                            for path in paths {
                                let _ = json_ext::insert_at_path(&mut placed, path, entity.clone());
                            }
                            if let Some(path) = first {
                                let _ = json_ext::insert_at_path(&mut placed, path, entity);
                            }
                        }
                    }
                    return (placed.seal(), errors);
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

            (Value::default(), errors)
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

                    let mut new_error = Error::builder()
                        .locations(error.locations.clone())
                        .path(path)
                        .message(error.message.clone())
                        .and_extension_code(error.extension_code())
                        .extensions(error.extensions.clone())
                        .apollo_id(error.apollo_id())
                        .build();
                    new_error.set_span_event_emitted(error.span_event_emitted());
                    new_error
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
        self.operation.init_parsed(schema)?;
        Ok(())
    }

    pub(super) fn init_parsed_operation_and_hash_subquery(
        &mut self,
        subgraph_schemas: &HashedSubgraphSchemas,
    ) -> Result<(), ValidationErrors> {
        let subgraph_schema = &subgraph_schemas[self.service_name.as_ref()];
        self.operation.init_parsed(&subgraph_schema.schema)?;
        self.schema_aware_hash = Arc::new(subgraph_schema.hash.operation_hash(
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
    use apollo_json::json;
    use rstest::rstest;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::plugins::connectors::declared_errors::ConnectorDeclaredErrors;
    use crate::plugins::connectors::declared_errors::DECLARED_ERROR_MARKER;

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
        json_ext::PathElement::Key(name.into(), None)
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
        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.as_ref().unwrap(), &expected_path);
    }

    #[test]
    fn representations_are_deduplicated_with_every_path_kept() {
        let schema = test_schema();
        let data = json!({"items": [
            {"__typename": "T", "id": "1", "other": true},
            {"__typename": "T", "id": "2"},
            {"__typename": "T", "id": "1"},
            {"__typename": "U", "id": "3"},
        ]});
        let request = Arc::new(http::Request::new(Request::default()));

        let variables = Variables::new(
            &make_requires(),
            &[],
            &data,
            &Path::from("items/@"),
            &request,
            &schema,
            &None,
            &None,
        )
        .expect("two entities match the type condition");

        assert_eq!(
            variables.variables.get("representations"),
            Some(json!([{"id": "1"}, {"id": "2"}]))
        );
        assert_eq!(
            *variables.inverted_paths,
            InvertedPaths::from(vec![
                vec![Path::from("items/0"), Path::from("items/2")],
                vec![Path::from("items/1")],
            ])
        );
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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            true,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

        assert!(errors.is_empty());
        let top = value.get("topField").unwrap();
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

        assert!(errors.is_empty());
        let arr = value.get("field").unwrap().as_array().unwrap();
        assert_eq!(arr[0], json!({"name": "Alice"}));
        assert_eq!(arr[2], json!({"name": "Alice"}));
    }

    /// A connector declares an error at a connector-local path, like
    /// `_entities/0/balance`. Only this rewrite turns that into paths a client
    /// can resolve, one per place the entity was fetched for. That is why
    /// declared errors ride in the `errors` array this far before the fetch
    /// service lifts them into the response `extensions`.
    ///
    /// Asserted here rather than in the connectors plugin because the rewrite
    /// belongs to the fetch node. If the entity branch ever stopped copying
    /// extensions, it would strip the marker that identifies these errors, and
    /// they would stay in `errors` where the spec says they must not be.
    #[test]
    fn entity_fetch_rewrites_paths_of_connector_declared_errors() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("accounts"), flatten()]);
        let inverted_paths = vec![vec![
            Path(vec![key("accounts"), index(0)]),
            Path(vec![key("accounts"), index(2)]),
        ]];
        let response = graphql::Response::builder()
            .data(json!({ "_entities": [{ "balance": 0 }] }))
            .error(
                graphql::Error::builder()
                    .message("balance unavailable")
                    .path(Path::from("_entities/0/balance"))
                    .extension_code("CONNECTORS_MAPPING_ERROR")
                    .extension(DECLARED_ERROR_MARKER, apollo_json::json!(true))
                    .build(),
            )
            .build();

        let (_value, mut errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

        // One error per path the entity landed at, each naming that path...
        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors
                .iter()
                .map(|error| error.path.as_ref().map(ToString::to_string))
                .collect::<Vec<_>>(),
            vec![
                Some("/accounts/0/balance".to_string()),
                Some("/accounts/2/balance".to_string()),
            ],
        );

        // ...and the marker survived the rebuild, so the hand-off still
        // recognizes them.
        let context = Context::new();
        ConnectorDeclaredErrors::take_marked(&context, &mut errors);
        assert!(errors.is_empty());
        assert_eq!(
            ConnectorDeclaredErrors::drain(&context)
                .as_ref()
                .and_then(Value::as_array)
                .map(|errors| errors.len()),
            Some(2),
        );
    }

    #[test]
    fn entity_fetch_empty_entities_array_returns_default_value() {
        let schema = test_schema();
        let node = make_fetch_node(make_requires());
        let current_dir = Path(vec![key("field")]);
        let response = graphql::Response::builder()
            .data(json!({"_entities": []}))
            .build();

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

        assert!(errors.is_empty());
        let arr = value.get("f").unwrap().as_array().unwrap();
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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

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
            &InvertedPaths::from(vec![vec![Path(vec![key("data"), index(0)])]]),
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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            false,
        );

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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

        assert_eq!(value, Value::default());
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

        assert_eq!(value, Value::default());
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

        assert_eq!(value, Value::default());
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            true,
        );

        assert_eq!(value, Value::default());
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            true,
        );

        assert_eq!(value, Value::default());
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            false,
        );

        assert_eq!(value, Value::default());
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

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            true,
        );

        assert_eq!(value, Value::default());
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
            data: Some(Value::default()),
            errors: vec![graphql::Error::builder().message("null data error").build()],
            ..Default::default()
        };

        let (value, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::default(),
            response,
            true,
        );

        assert_eq!(value, Value::default());
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

        let (_, errors) = node.response_at_path(
            &schema,
            &current_dir,
            &InvertedPaths::from(inverted_paths),
            response,
            true,
        );

        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors[0].path.as_ref().unwrap(),
            &Path(vec![key("users"), index(1), key("field")])
        );
        assert_eq!(errors[1].path.as_ref().unwrap(), &Path(vec![key("users")]));
        assert_eq!(errors[2].path.as_ref().unwrap(), &Path(vec![key("users")]));
    }

    /// Regression test for the representation dedup that connector `$batch`
    /// (and plain `_entities` fetches) rely on: identical representations
    /// collapse to one entry in `representations`, in order of first
    /// appearance, and `inverted_paths` remembers every path that produced
    /// each entry so the single returned entity can be fanned back out.
    #[test]
    fn variables_dedupe_identical_representations_and_track_all_paths() {
        let schema = test_schema();
        let request = Arc::new(
            http::Request::builder()
                .body(graphql::Request::builder().build())
                .unwrap(),
        );
        let data = json!({
            "t": [
                { "__typename": "T", "id": "1", "extra": "ignored" },
                { "__typename": "T", "id": "2" },
                { "__typename": "T", "id": "1", "extra": "also ignored" },
            ]
        });
        let current_dir = Path(vec![key("t"), flatten()]);

        let variables = Variables::new(
            &make_requires(),
            &[],
            &data,
            &current_dir,
            &request,
            &schema,
            &None,
            &None,
        )
        .expect("at least one representation");

        // Three entities in the parent data, two distinct keys. Fields outside
        // the `requires` selection (`extra`) play no part in equality.
        assert_eq!(
            variables.variables.get("representations"),
            Some(apollo_json::json!([{ "id": "1" }, { "id": "2" }])),
        );

        // Representation 0 (`id: 1`) came from t/0 and t/2; representation 1
        // (`id: 2`) came from t/1 alone.
        assert_eq!(
            *variables.inverted_paths,
            InvertedPaths::from(vec![
                vec![
                    Path(vec![key("t"), index(0)]),
                    Path(vec![key("t"), index(2)]),
                ],
                vec![Path(vec![key("t"), index(1)])],
            ]),
        );
    }
}
