use std::num::NonZeroU32;
use std::sync::Arc;

use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::csr::NodeIndex;
use petgraph::stable_graph::IndexType;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::query_graph::build_federated_query_graph;
use crate::query_graph::path_tree::OpPathTree;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphNodeType;
use crate::query_plan::fetch_dependency_graph::compute_nodes_for_tree;
use crate::query_plan::fetch_dependency_graph::FetchDependencyGraph;
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphProcessor;
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphToCostProcessor;
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphToQueryPlanProcessor;
use crate::query_plan::operation::normalize_operation;
use crate::query_plan::operation::NamedFragments;
use crate::query_plan::operation::NormalizedDefer;
use crate::query_plan::operation::RebasedFragments;
use crate::query_plan::operation::SelectionSet;
use crate::query_plan::query_planning_traversal::BestQueryPlanInfo;
use crate::query_plan::query_planning_traversal::QueryPlanningParameters;
use crate::query_plan::query_planning_traversal::QueryPlanningTraversal;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::SequenceNode;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::ApiSchemaOptions;
use crate::Supergraph;

#[derive(Debug, Clone, Hash)]
pub struct QueryPlannerConfig {
    /// Whether the query planner should try to reused the named fragments of the planned query in
    /// subgraph fetches.
    ///
    /// This is often a good idea as it can prevent very large subgraph queries in some cases (named
    /// fragments can make some relatively small queries (using said fragments) expand to a very large
    /// query if all the spreads are inline). However, due to architecture of the query planner, this
    /// optimization is done as an additional pass on the subgraph queries of the generated plan and
    /// can thus increase the latency of building a plan. As long as query plans are sufficiently
    /// cached, this should not be a problem, which is why this option is enabled by default, but if
    /// the distribution of inbound queries prevents efficient caching of query plans, this may become
    /// an undesirable trade-off and can be disabled in that case.
    ///
    /// Defaults to true.
    pub reuse_query_fragments: bool,

    /// Whether to run GraphQL validation against the extracted subgraph schemas. Recommended in
    /// non-production settings or when debugging.
    ///
    /// Defaults to false.
    pub subgraph_graphql_validation: bool,

    // Side-note: implemented as an object instead of single boolean because we expect to add more
    // to this soon enough. In particular, once defer-passthrough to subgraphs is implemented, the
    // idea would be to add a new `passthrough_subgraphs` option that is the list of subgraphs to
    // which we can pass-through some @defer (and it would be empty by default). Similarly, once we
    // support @stream, grouping the options here will make sense too.
    pub incremental_delivery: QueryPlanIncrementalDeliveryConfig,

    /// A sub-set of configurations that are meant for debugging or testing. All the configurations
    /// in this sub-set are provided without guarantees of stability (they may be dangerous) or
    /// continued support (they may be removed without warning).
    pub debug: QueryPlannerDebugConfig,
}

impl Default for QueryPlannerConfig {
    fn default() -> Self {
        Self {
            reuse_query_fragments: true,
            subgraph_graphql_validation: false,
            incremental_delivery: Default::default(),
            debug: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Hash)]
pub struct QueryPlanIncrementalDeliveryConfig {
    /// Enables @defer support by the query planner.
    ///
    /// If set, then the query plan for queries having some @defer will contains some `DeferNode`
    /// (see `query_plan/mod.rs`).
    ///
    /// Defaults to false (meaning that the @defer are ignored).
    pub enable_defer: bool,
}

#[derive(Debug, Clone, Hash)]
pub struct QueryPlannerDebugConfig {
    /// If used and the supergraph is built from a single subgraph, then user queries do not go
    /// through the normal query planning and instead a fetch to the one subgraph is built directly
    /// from the input query.
    pub bypass_planner_for_single_subgraph: bool,

    /// Query planning is an exploratory process. Depending on the specificities and feature used by
    /// subgraphs, there could exist may different theoretical valid (if not always efficient) plans
    /// for a given query, and at a high level, the query planner generates those possible choices,
    /// evaluates them, and return the best one. In some complex cases however, the number of
    /// theoretically possible plans can be very large, and to keep query planning time acceptable,
    /// the query planner caps the maximum number of plans it evaluates. This config allows to
    /// configure that cap. Note if planning a query hits that cap, then the planner will still
    /// always return a "correct" plan, but it may not return _the_ optimal one, so this config can
    /// be considered a trade-off between the worst-time for query planning computation processing,
    /// and the risk of having non-optimal query plans (impacting query runtimes).
    ///
    /// This value currently defaults to 10000, but this default is considered an implementation
    /// detail and is subject to change. We do not recommend setting this value unless it is to
    /// debug a specific issue (with unexpectedly slow query planning for instance). Remember that
    /// setting this value too low can negatively affect query runtime (due to the use of
    /// sub-optimal query plans).
    // TODO: should there additionally be a max_evaluated_cost?
    pub max_evaluated_plans: NonZeroU32,

    /// Before creating query plans, for each path of fields in the query we compute all the
    /// possible options to traverse that path via the subgraphs. Multiple options can arise because
    /// fields in the path can be provided by multiple subgraphs, and abstract types (i.e. unions
    /// and interfaces) returned by fields sometimes require the query planner to traverse through
    /// each constituent object type. The number of options generated in this computation can grow
    /// large if the schema or query are sufficiently complex, and that will increase the time spent
    /// planning.
    ///
    /// This config allows specifying a per-path limit to the number of options considered. If any
    /// path's options exceeds this limit, query planning will abort and the operation will fail.
    ///
    /// The default value is None, which specifies no limit.
    pub paths_limit: Option<u32>,
}

impl Default for QueryPlannerDebugConfig {
    fn default() -> Self {
        Self {
            bypass_planner_for_single_subgraph: false,
            max_evaluated_plans: NonZeroU32::new(10_000).unwrap(),
            paths_limit: None,
        }
    }
}

// PORT_NOTE: renamed from PlanningStatistics in the JS codebase.
#[derive(Debug, Default, Clone)]
pub struct QueryPlanningStatistics {
    pub evaluated_plan_count: usize,
}

impl QueryPlannerConfig {
    /// Panics if options are used together in unsupported ways.
    fn assert_valid(&self) {
        if self.incremental_delivery.enable_defer {
            assert!(!self.debug.bypass_planner_for_single_subgraph, "Cannot use the `debug.bypass_planner_for_single_subgraph` query planner option when @defer support is enabled");
        }
    }
}

pub struct QueryPlanner {
    config: QueryPlannerConfig,
    federated_query_graph: Arc<QueryGraph>,
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    subgraph_federation_spec_definitions: Arc<IndexMap<NodeStr, &'static FederationSpecDefinition>>,
    /// A set of the names of interface types for which at least one subgraph use an
    /// @interfaceObject to abstract that interface.
    interface_types_with_interface_objects: IndexSet<InterfaceTypeDefinitionPosition>,
    /// A set of the names of interface or union types that have inconsistent "runtime types" across
    /// subgraphs.
    // PORT_NOTE: Named `inconsistentAbstractTypesRuntimes` in the JS codebase, which was slightly
    // confusing.
    abstract_types_with_inconsistent_runtime_types: IndexSet<AbstractTypeDefinitionPosition>,
}

impl QueryPlanner {
    pub fn new(
        supergraph: &Supergraph,
        config: QueryPlannerConfig,
    ) -> Result<Self, FederationError> {
        config.assert_valid();

        let supergraph_schema = supergraph.schema.clone();
        let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
            include_defer: config.incremental_delivery.enable_defer,
            ..Default::default()
        })?;
        let query_graph = build_federated_query_graph(
            supergraph_schema.clone(),
            api_schema.clone(),
            Some(true),
            Some(true),
        )?;

        let metadata = supergraph_schema.metadata().unwrap();

        let federation_link = metadata.for_identity(&Identity::federation_identity());
        let interface_object_directive =
            federation_link.map_or(FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC, |link| {
                link.directive_name_in_schema(&FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC)
            });

        let is_interface_object =
            |ty: &ExtendedType| ty.is_object() && ty.directives().has(&interface_object_directive);

        let interface_types_with_interface_objects = supergraph
            .schema
            .get_types()
            .filter_map(|position| match position {
                TypeDefinitionPosition::Interface(interface_position) => Some(interface_position),
                _ => None,
            })
            .filter(|position| {
                query_graph.sources().any(|(_name, schema)| {
                    schema
                        .schema()
                        .types
                        .get(&position.type_name)
                        .is_some_and(is_interface_object)
                })
            })
            .collect::<IndexSet<_>>();

        let is_inconsistent = |position: AbstractTypeDefinitionPosition| {
            let mut sources = query_graph.sources().filter_map(|(_name, subgraph)| {
                match subgraph.try_get_type(position.type_name().clone())? {
                    // This is only called for type names that are abstract in the supergraph, so it
                    // can only be an object in a subgraph if it is an `@interfaceObject`. And as `@interfaceObject`s
                    // "stand-in" for all possible runtime types, they don't create inconsistencies by themselves
                    // and we can ignore them.
                    TypeDefinitionPosition::Object(_) => None,
                    TypeDefinitionPosition::Interface(interface) => Some(
                        subgraph
                            .referencers()
                            .get_interface_type(&interface.type_name)
                            .ok()?
                            .object_types
                            .clone(),
                    ),
                    TypeDefinitionPosition::Union(union_) => Some(
                        union_
                            .try_get(subgraph.schema())?
                            .members
                            .iter()
                            .map(|member| ObjectTypeDefinitionPosition::new(member.name.clone()))
                            .collect(),
                    ),
                    _ => None,
                }
            });

            let Some(expected_runtimes) = sources.next() else {
                return false;
            };
            sources.all(|runtimes| runtimes == expected_runtimes)
        };

        let abstract_types_with_inconsistent_runtime_types = supergraph
            .schema
            .get_types()
            .filter_map(|position| AbstractTypeDefinitionPosition::try_from(position).ok())
            .filter(|position| is_inconsistent(position.clone()))
            .collect::<IndexSet<_>>();

        // PORT_NOTE: JS prepares a map of override conditions here, which is
        // a map where the keys are all `@join__field(overrideLabel:)` argument values
        // and the values are all initialised to `false`. Instead of doing that, we should
        // be able to use a Set where presence means `true` and absence means `false`.

        Ok(Self {
            config,
            federated_query_graph: Arc::new(query_graph),
            supergraph_schema,
            api_schema,
            // TODO(@goto-bus-stop): not sure how this is going to be used,
            // keeping empty for the moment
            subgraph_federation_spec_definitions: Default::default(),
            interface_types_with_interface_objects,
            abstract_types_with_inconsistent_runtime_types,
        })
    }

    pub fn subgraph_schemas(&self) -> &IndexMap<NodeStr, ValidFederationSchema> {
        &self.federated_query_graph.sources
    }

    // PORT_NOTE: this receives an `Operation` object in JS which is a concept that doesn't exist in apollo-rs.
    pub fn build_query_plan(
        &self,
        document: &Valid<ExecutableDocument>,
        operation_name: Option<Name>,
    ) -> Result<QueryPlan, FederationError> {
        let operation = document
            .get_operation(operation_name.as_ref().map(|name| name.as_str()))
            // TODO(@goto-bus-stop) this is not an internal error, but a user error
            .map_err(|_| FederationError::internal("requested operation does not exist"))?;

        if operation.selection_set.selections.is_empty() {
            // This should never happen because `operation` comes from a known-valid document.
            return Err(SingleFederationError::InvalidGraphQL {
                message: "Invalid operation: empty selection set".to_string(),
            }
            .into());
        }

        let is_subscription = operation.is_subscription();

        let statistics = QueryPlanningStatistics {
            evaluated_plan_count: 0,
        };

        if self.config.debug.bypass_planner_for_single_subgraph {
            // A federated query graph always have 1 more sources than there is subgraph, because the root vertices
            // belong to no subgraphs and use a special source named '_'. So we skip that "fake" source.
            let mut subgraphs = self
                .federated_query_graph
                .sources()
                .filter(|&(name, _schema)| name != "_");
            if let (Some((subgraph_name, _subgraph_schema)), None) =
                (subgraphs.next(), subgraphs.next())
            {
                let node = FetchNode {
                    subgraph_name: subgraph_name.clone(),
                    operation_document: document.clone(),
                    operation_name: operation.name.as_deref().cloned(),
                    operation_kind: operation.operation_type,
                    id: None,
                    variable_usages: operation
                        .variables
                        .iter()
                        .map(|var| var.name.clone())
                        .collect(),
                    requires: Default::default(),
                    input_rewrites: Default::default(),
                    output_rewrites: Default::default(),
                    context_rewrites: Default::default(),
                };

                return Ok(QueryPlan::new(node, statistics));
            }
        }

        let reuse_query_fragments = self.config.reuse_query_fragments;
        let normalized_operation = normalize_operation(
            operation,
            NamedFragments::new(&document.fragments, &self.api_schema),
            &self.api_schema,
            &self.interface_types_with_interface_objects,
        )?;

        let (normalized_operation, assigned_defer_labels, defer_conditions, has_defers) =
            if self.config.incremental_delivery.enable_defer {
                let NormalizedDefer {
                    operation,
                    assigned_defer_labels,
                    defer_conditions,
                    has_defers,
                } = normalized_operation.with_normalized_defer();
                if has_defers && is_subscription {
                    return Err(SingleFederationError::DeferredSubscriptionUnsupported.into());
                }
                (
                    operation,
                    Some(assigned_defer_labels),
                    Some(defer_conditions),
                    has_defers,
                )
            } else {
                // If defer is not enabled, we remove all @defer from the query. This feels cleaner do this once here than
                // having to guard all the code dealing with defer later, and is probably less error prone too (less likely
                // to end up passing through a @defer to a subgraph by mistake).
                (normalized_operation.without_defer(), None, None, false)
            };

        if normalized_operation.selection_set.selections.is_empty() {
            return Ok(QueryPlan::default());
        }

        let Some(root) = self
            .federated_query_graph
            .root_kinds_to_nodes()?
            .get(&normalized_operation.root_kind)
        else {
            panic!(
                "Shouldn't have a {0} operation if the subgraphs don't have a {0} root",
                normalized_operation.root_kind
            );
        };

        let rebased_fragments = if reuse_query_fragments {
            // For all subgraph fetches we query `__typename` on every abstract types (see
            // `FetchDependencyGraphNode::to_plan_node`) so if we want to have a chance to reuse
            // fragments, we should make sure those fragments also query `__typename` for every
            // abstract type.
            Some(RebasedFragments::new(
                normalized_operation
                    .named_fragments
                    .add_typename_field_for_abstract_types_in_named_fragments()?,
            ))
        } else {
            None
        };
        let processor = FetchDependencyGraphToQueryPlanProcessor::new(
            operation.variables.clone(),
            rebased_fragments,
            operation.name.clone(),
            assigned_defer_labels,
        );
        let mut parameters = QueryPlanningParameters {
            supergraph_schema: self.supergraph_schema.clone(),
            federated_query_graph: self.federated_query_graph.clone(),
            operation: Arc::new(normalized_operation),
            processor,
            head: *root,
            // PORT_NOTE(@goto-bus-stop): In JS, `root` is a `RootVertex`, which is dynamically
            // checked at various points in query planning. This is our Rust equivalent of that.
            head_must_be_root: true,
            statistics,
            abstract_types_with_inconsistent_runtime_types: self
                .abstract_types_with_inconsistent_runtime_types
                .clone()
                .into(),
            config: self.config.clone(),
            // PORT_NOTE: JS provides `override_conditions` here: see port note in `QueryPlanner::new`.
        };

        let root_node = match defer_conditions {
            Some(defer_conditions) if !defer_conditions.is_empty() => {
                compute_plan_for_defer_conditionals(&mut parameters, defer_conditions)?
            }
            _ => compute_plan_internal(&mut parameters, has_defers)?,
        };

        let root_node = match root_node {
            // If this is a subscription, we want to make sure that we return a SubscriptionNode rather than a PlanNode
            // We potentially will need to separate "primary" from "rest"
            // Note that if it is a subscription, we are guaranteed that nothing is deferred.
            Some(PlanNode::Fetch(root_node)) if is_subscription => Some(
                TopLevelPlanNode::Subscription(crate::query_plan::SubscriptionNode {
                    primary: root_node,
                    rest: None,
                }),
            ),
            Some(PlanNode::Sequence(root_node)) if is_subscription => {
                let Some((primary, rest)) = root_node.nodes.split_first() else {
                    unreachable!("Sequence must have at least one node");
                };
                let PlanNode::Fetch(primary) = primary.clone() else {
                    unreachable!("Primary node of a subscription is not a Fetch");
                };
                let rest = PlanNode::Sequence(SequenceNode {
                    nodes: rest.to_vec(),
                });
                Some(TopLevelPlanNode::Subscription(
                    crate::query_plan::SubscriptionNode {
                        primary,
                        rest: Some(Box::new(rest)),
                    },
                ))
            }
            Some(node) if is_subscription => {
                unreachable!(
                    "Unexpected top level PlanNode: '{node:?}' when processing subscription"
                )
            }
            Some(PlanNode::Fetch(inner)) => Some(TopLevelPlanNode::Fetch(inner)),
            Some(PlanNode::Sequence(inner)) => Some(TopLevelPlanNode::Sequence(inner)),
            Some(PlanNode::Parallel(inner)) => Some(TopLevelPlanNode::Parallel(inner)),
            Some(PlanNode::Flatten(inner)) => Some(TopLevelPlanNode::Flatten(inner)),
            Some(PlanNode::Defer(inner)) => Some(TopLevelPlanNode::Defer(inner)),
            Some(PlanNode::Condition(inner)) => Some(TopLevelPlanNode::Condition(inner)),
            None => None,
        };

        Ok(QueryPlan {
            node: root_node,
            statistics: parameters.statistics,
        })
    }

    /// Get Query Planner's API Schema.
    pub fn api_schema(&self) -> &ValidFederationSchema {
        &self.api_schema
    }
}

fn compute_root_serial_dependency_graph(
    parameters: &QueryPlanningParameters,
    has_defers: bool,
) -> Result<Vec<FetchDependencyGraph>, FederationError> {
    let QueryPlanningParameters {
        supergraph_schema,
        federated_query_graph,
        operation,
        ..
    } = parameters;
    let root_type: Option<CompositeTypeDefinitionPosition> = if has_defers {
        supergraph_schema
            .schema()
            .root_operation(operation.root_kind.into())
            .and_then(|name| supergraph_schema.get_type(name.clone()).ok())
            .and_then(|ty| ty.try_into().ok())
    } else {
        None
    };
    // We have to serially compute a plan for each top-level selection.
    let mut split_roots = operation.selection_set.clone().split_top_level_fields();
    let mut digest = Vec::new();
    let mut starting_fetch_id = 0;
    let selection_set = split_roots
        .next()
        .ok_or_else(|| FederationError::internal("Empty top level fields"))?;
    let BestQueryPlanInfo {
        mut fetch_dependency_graph,
        path_tree: mut prev_path,
        ..
    } = compute_root_parallel_best_plan(parameters, selection_set, has_defers)?;
    let mut prev_subgraph = only_root_subgraph(&fetch_dependency_graph)?;
    for selection_set in split_roots {
        let BestQueryPlanInfo {
            fetch_dependency_graph: new_dep_graph,
            path_tree: new_path,
            ..
        } = compute_root_parallel_best_plan(parameters, selection_set, has_defers)?;
        let new_subgraph = only_root_subgraph(&new_dep_graph)?;
        if new_subgraph == prev_subgraph {
            // The new operation (think 'mutation' operation) is on the same subgraph than the previous one, so we can concat them in a single fetch
            // and rely on the subgraph to enforce seriability. Do note that we need to `concat()` and not `merge()` because if we have
            // mutation Mut {
            //    mut1 {...}
            //    mut2 {...}
            //    mut1 {...}
            // }
            // then we should _not_ merge the 2 `mut1` fields (contrarily to what happens on queried fields).

            prev_path = OpPathTree::merge(&prev_path, &new_path);
            fetch_dependency_graph = FetchDependencyGraph::new(
                supergraph_schema.clone(),
                federated_query_graph.clone(),
                root_type.clone(),
                starting_fetch_id,
            );
            compute_root_fetch_groups(
                operation.root_kind,
                &mut fetch_dependency_graph,
                &prev_path,
            )?;
        } else {
            // PORT_NOTE: It is unclear if they correct thing to do here is get the next ID, use
            // the current ID that is inside the fetch dep graph's ID generator, or to use the
            // starting ID. Because this method ensure uniqueness between IDs, this approach was
            // taken; however, it could be the case that this causes unforseen issues.
            starting_fetch_id = fetch_dependency_graph.next_fetch_id();
            digest.push(std::mem::replace(
                &mut fetch_dependency_graph,
                new_dep_graph,
            ));
            prev_path = new_path;
            prev_subgraph = new_subgraph;
        }
    }
    digest.push(fetch_dependency_graph);
    Ok(digest)
}

fn only_root_subgraph(graph: &FetchDependencyGraph) -> Result<NodeIndex, FederationError> {
    let mut iter = graph.root_node_by_subgraph_iter();
    let (Some((_, index)), None) = (iter.next(), iter.next()) else {
        return Err(FederationError::internal(format!(
            "{graph} should have only one root."
        )));
    };
    Ok(index.index() as u32)
}

pub(crate) fn compute_root_fetch_groups(
    root_kind: SchemaRootDefinitionKind,
    dependency_graph: &mut FetchDependencyGraph,
    path: &OpPathTree,
) -> Result<(), FederationError> {
    // The root of the pathTree is one of the "fake" root of the subgraphs graph,
    // which belongs to no subgraph but points to each ones.
    // So we "unpack" the first level of the tree to find out our top level groups
    // (and initialize our stack).
    // Note that we can safely ignore the triggers of that first level
    // as it will all be free transition, and we know we cannot have conditions.
    for child in &path.childs {
        let edge = child.edge.expect("The root edge should not be None");
        let (_source_node, target_node) = path.graph.edge_endpoints(edge)?;
        let target_node = path.graph.node_weight(target_node)?;
        let subgraph_name = &target_node.source;
        let root_type = match &target_node.type_ {
            QueryGraphNodeType::SchemaType(OutputTypeDefinitionPosition::Object(object)) => {
                object.clone().into()
            }
            ty => {
                return Err(FederationError::internal(format!(
                    "expected an object type for the root of a subgraph, found {ty}"
                )))
            }
        };
        let fetch_dependency_node =
            dependency_graph.get_or_create_root_node(subgraph_name, root_kind, root_type)?;
        compute_nodes_for_tree(
            dependency_graph,
            &child.tree,
            fetch_dependency_node,
            Default::default(),
            Default::default(),
            &Default::default(),
        )?;
    }
    Ok(())
}

fn compute_root_parallel_dependency_graph(
    parameters: &QueryPlanningParameters,
    has_defers: bool,
) -> Result<FetchDependencyGraph, FederationError> {
    let selection_set = parameters.operation.selection_set.clone();
    let best_plan = compute_root_parallel_best_plan(parameters, selection_set, has_defers)?;
    Ok(best_plan.fetch_dependency_graph)
}

fn compute_root_parallel_best_plan(
    parameters: &QueryPlanningParameters,
    selection: SelectionSet,
    has_defers: bool,
) -> Result<BestQueryPlanInfo, FederationError> {
    let planning_traversal = QueryPlanningTraversal::new(
        parameters,
        selection,
        has_defers,
        parameters.operation.root_kind,
        FetchDependencyGraphToCostProcessor,
    )?;

    // Getting no plan means the query is essentially unsatisfiable (it's a valid query, but we can prove it will never return a result),
    // so we just return an empty plan.
    Ok(planning_traversal
        .find_best_plan()?
        .unwrap_or_else(|| BestQueryPlanInfo::empty(parameters)))
}

fn compute_plan_internal(
    parameters: &mut QueryPlanningParameters,
    has_defers: bool,
) -> Result<Option<PlanNode>, FederationError> {
    let root_kind = parameters.operation.root_kind;

    let (main, deferred, primary_selection) = if root_kind == SchemaRootDefinitionKind::Mutation {
        let dependency_graphs = compute_root_serial_dependency_graph(parameters, has_defers)?;
        let mut main = None;
        let mut deferred = vec![];
        let mut primary_selection = None::<SelectionSet>;
        for mut dependency_graph in dependency_graphs {
            let (local_main, local_deferred) =
                dependency_graph.process(&mut parameters.processor, root_kind)?;
            main = match main {
                Some(unlocal_main) => parameters
                    .processor
                    .reduce_sequence([Some(unlocal_main), local_main]),
                None => local_main,
            };
            deferred.extend(local_deferred);
            let new_selection = dependency_graph.defer_tracking.primary_selection;
            match primary_selection.as_mut() {
                Some(selection) => selection.merge_into(new_selection.iter())?,
                None => primary_selection = new_selection,
            }
        }
        (main, deferred, primary_selection)
    } else {
        let mut dependency_graph = compute_root_parallel_dependency_graph(parameters, has_defers)?;

        let (main, deferred) = dependency_graph.process(&mut parameters.processor, root_kind)?;
        // XXX(@goto-bus-stop) Maybe `.defer_tracking` should be on the return value of `process()`..?
        let primary_selection = dependency_graph.defer_tracking.primary_selection;

        (main, deferred, primary_selection)
    };

    if deferred.is_empty() {
        Ok(main)
    } else {
        let Some(primary_selection) = primary_selection else {
            unreachable!("Should have had a primary selection created");
        };
        parameters
            .processor
            .reduce_defer(main, &primary_selection, deferred)
    }
}

fn compute_plan_for_defer_conditionals(
    _parameters: &mut QueryPlanningParameters,
    _defer_conditions: IndexMap<String, IndexSet<String>>,
) -> Result<Option<PlanNode>, FederationError> {
    todo!("FED-95")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgraph::Subgraph;

    const TEST_SUPERGRAPH: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
{
  query: Query
}

directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

type Book implements Product
  @join__implements(graph: PRODUCTS, interface: "Product")
  @join__implements(graph: REVIEWS, interface: "Product")
  @join__type(graph: PRODUCTS, key: "id")
  @join__type(graph: REVIEWS, key: "id")
{
  id: ID!
  price: Price @join__field(graph: PRODUCTS)
  title: String @join__field(graph: PRODUCTS)
  vendor: User @join__field(graph: PRODUCTS)
  pages: Int @join__field(graph: PRODUCTS)
  avg_rating: Int @join__field(graph: PRODUCTS, requires: "reviews { rating }")
  reviews: [Review] @join__field(graph: PRODUCTS, external: true) @join__field(graph: REVIEWS)
}

enum Currency
  @join__type(graph: PRODUCTS)
{
  USD
  EUR
}

scalar join__FieldSet

enum join__Graph {
  ACCOUNTS @join__graph(name: "accounts", url: "")
  PRODUCTS @join__graph(name: "products", url: "")
  REVIEWS @join__graph(name: "reviews", url: "")
}

scalar link__Import

enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY

  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}

type Movie implements Product
  @join__implements(graph: PRODUCTS, interface: "Product")
  @join__implements(graph: REVIEWS, interface: "Product")
  @join__type(graph: PRODUCTS, key: "id")
  @join__type(graph: REVIEWS, key: "id")
{
  id: ID!
  price: Price @join__field(graph: PRODUCTS)
  title: String @join__field(graph: PRODUCTS)
  vendor: User @join__field(graph: PRODUCTS)
  length_minutes: Int @join__field(graph: PRODUCTS)
  avg_rating: Int @join__field(graph: PRODUCTS, requires: "reviews { rating }")
  reviews: [Review] @join__field(graph: PRODUCTS, external: true) @join__field(graph: REVIEWS)
}

type Price
  @join__type(graph: PRODUCTS)
{
  value: Int
  currency: Currency
}

interface Product
  @join__type(graph: PRODUCTS)
  @join__type(graph: REVIEWS)
{
  id: ID!
  price: Price @join__field(graph: PRODUCTS)
  vendor: User @join__field(graph: PRODUCTS)
  avg_rating: Int @join__field(graph: PRODUCTS)
  reviews: [Review] @join__field(graph: REVIEWS)
}

type Query
  @join__type(graph: ACCOUNTS)
  @join__type(graph: PRODUCTS)
  @join__type(graph: REVIEWS)
{
  userById(id: ID!): User @join__field(graph: ACCOUNTS)
  me: User! @join__field(graph: ACCOUNTS) @join__field(graph: REVIEWS)
  productById(id: ID!): Product @join__field(graph: PRODUCTS)
  search(filter: SearchFilter): [Product] @join__field(graph: PRODUCTS)
  bestRatedProducts(limit: Int): [Product] @join__field(graph: REVIEWS)
}

type Review
  @join__type(graph: PRODUCTS)
  @join__type(graph: REVIEWS)
{
  rating: Int @join__field(graph: PRODUCTS, external: true) @join__field(graph: REVIEWS)
  product: Product @join__field(graph: REVIEWS)
  author: User @join__field(graph: REVIEWS)
  text: String @join__field(graph: REVIEWS)
}

input SearchFilter
  @join__type(graph: PRODUCTS)
{
  pattern: String!
  vendorName: String
}

type User
  @join__type(graph: ACCOUNTS, key: "id")
  @join__type(graph: PRODUCTS, key: "id", resolvable: false)
  @join__type(graph: REVIEWS, key: "id")
{
  id: ID!
  name: String @join__field(graph: ACCOUNTS)
  email: String @join__field(graph: ACCOUNTS)
  password: String @join__field(graph: ACCOUNTS)
  nickname: String @join__field(graph: ACCOUNTS, override: "reviews")
  reviews: [Review] @join__field(graph: REVIEWS)
}
    "#;

    #[test]
    fn plan_simple_query_for_single_subgraph() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();

        let document = ExecutableDocument::parse_and_validate(
            planner.api_schema().schema(),
            r#"
            {
                userById(id: 1) {
                    name
                    email
                }
            }
            "#,
            "operation.graphql",
        )
        .unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Fetch(service: "accounts") {
            {
              userById(id: 1) {
                name
                email
              }
            }
          },
        }
        "###);
    }

    #[test]
    fn plan_simple_query_for_multiple_subgraphs() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();

        let document = ExecutableDocument::parse_and_validate(
            planner.api_schema().schema(),
            r#"
            {
                bestRatedProducts {
                    vendor { name }
                }
            }
            "#,
            "operation.graphql",
        )
        .unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        // TODO: This is the current output, but it's wrong: it's not fetching `vendor.name` at all.
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Sequence {
            Fetch(service: "reviews") {
              {
                bestRatedProducts {
                  ... on Book {
                    __typename
                    id
                  }
                  ... on Movie {
                    __typename
                    id
                  }
                }
              }
            },
            Parallel {
              Sequence {
                Flatten(path: "bestRatedProducts.@") {
                  Fetch(service: "products") {
                    {
                      ... on Movie {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on Movie {
                        vendor {
                          __typename
                          id
                        }
                      }
                    }
                  },
                },
                Flatten(path: "bestRatedProducts.@.vendor") {
                  Fetch(service: "accounts") {
                    {
                      ... on User {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on User {
                        name
                      }
                    }
                  },
                },
              },
              Sequence {
                Flatten(path: "bestRatedProducts.@") {
                  Fetch(service: "products") {
                    {
                      ... on Book {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on Book {
                        vendor {
                          __typename
                          id
                        }
                      }
                    }
                  },
                },
                Flatten(path: "bestRatedProducts.@.vendor") {
                  Fetch(service: "accounts") {
                    {
                      ... on User {
                        __typename
                        id
                      }
                    } =>
                    {
                      ... on User {
                        name
                      }
                    }
                  },
                },
              },
            },
          },
        }
        "###);
    }

    // TODO: This fails with "Subgraph unexpectedly does not use federation spec"
    // which seems...unusual
    #[test]
    #[ignore]
    fn plan_simple_root_field_query_for_multiple_subgraphs() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();

        let document = ExecutableDocument::parse_and_validate(
            planner.api_schema().schema(),
            r#"
            {
                userById(id: 1) {
                    name
                    email
                }
                bestRatedProducts {
                    id
                    avg_rating
                }
            }
            "#,
            "operation.graphql",
        )
        .unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Parallel {
            Fetch(service: "accounts") {
              {
                      userById(id: 1) {
                  name
                  email
                }
              }
            }
            Fetch(service: "products") {
              {
                      bestRatedProducts {
                  id
                  avg_rating
                }
              }
            }
          }
        }
        "###);
    }

    #[test]
    fn bypass_planner_for_single_subgraph() {
        let a = Subgraph::parse_and_expand(
            "A",
            "https://A",
            r#"
            type Query {
                a: A
            }
            type A {
                b: B
            }
            type B {
                x: Int
                y: String
            }
        "#,
        )
        .unwrap();
        let subgraphs = vec![&a];
        let supergraph = Supergraph::compose(subgraphs).unwrap();
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();

        let document = ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            r#"
            {
                a {
                    b {
                        x
                        y
                    }
                }
            }
            "#,
            "",
        )
        .unwrap();

        let mut config = QueryPlannerConfig::default();
        config.debug.bypass_planner_for_single_subgraph = true;
        let planner = QueryPlanner::new(&supergraph, config).unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Fetch(service: "A") {
            {
              a {
                b {
                  x
                  y
                }
              }
            }
          },
        }
        "###);
    }

    #[test]
    fn test_optimize_basic() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
        let document = ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            r#"
            {
                userById(id: 1) {
                    id
                    ...userFields
                },
                another_user: userById(id: 2) {
                  name
                  email
              }
            }
            fragment userFields on User {
                name
                email
            }
            "#,
            "operation.graphql",
        )
            .unwrap();

        let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Fetch(service: "accounts") {
            {
              userById(id: 1) {
                ...userFields
                id
              }      another_user: userById(id: 2) {
                ...userFields
              }
            }

            fragment userFields on User {
              name
              email
            }
          },
        }
        "###);
    }

    #[test]
    fn test_optimize_inline_fragment() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
        let document = ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            r#"
            {
                userById(id: 1) {
                    id
                    ...userFields
                },
                partial_optimize: userById(id: 2) {
                    ... on User {
                        id
                        name
                        email
                    }
                },
                full_optimize: userById(id: 3) {
                    ... on User {
                        name
                        email
                    }
                }
            }
            fragment userFields on User {
                name
                email
            }
            "#,
            "operation.graphql",
        )
            .unwrap();

        let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Fetch(service: "accounts") {
            {
              userById(id: 1) {
                ...userFields
                id
              }      partial_optimize: userById(id: 2) {
                ...userFields
                id
              }      full_optimize: userById(id: 3) {
                ...userFields
              }
            }

            fragment userFields on User {
              name
              email
            }
          },
        }
        "###);
    }

    #[test]
    fn test_optimize_fragment_definition() {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH).unwrap();
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
        let document = ExecutableDocument::parse_and_validate(
            api_schema.schema(),
            r#"
            {
                userById(id: 1) {
                    ...F1
                    ...F2
                },
                case2: userById(id: 2) {
                    id
                    name
                    email
                },
            }
            fragment F1 on User {
                name
                email
            }
            fragment F2 on User {
                id
                name
                email
            }
            "#,
            "operation.graphql",
        )
            .unwrap();

        let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();
        let plan = planner.build_query_plan(&document, None).unwrap();
        // Make sure `fragment F2` contains `...F1`.
        insta::assert_snapshot!(plan, @r###"
        QueryPlan {
          Fetch(service: "accounts") {
            {
              userById(id: 1) {
                ...F2
              }      case2: userById(id: 2) {
                ...F2
              }
            }

            fragment F2 on User {
              name
              email
              id
            }
          },
        }
        "###);
    }
}
