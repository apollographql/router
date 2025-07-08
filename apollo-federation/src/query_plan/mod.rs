use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable;
use serde::Deserialize;
use serde::Serialize;

use crate::query_plan::query_planner::QueryPlanningStatistics;

pub(crate) mod conditions;
pub(crate) mod display;
pub(crate) mod fetch_dependency_graph;
pub(crate) mod fetch_dependency_graph_processor;
pub mod generate;
pub mod query_planner;
pub(crate) mod query_planning_traversal;
pub mod requires_selection;
pub mod serializable_document;

pub type QueryPlanCost = f64;

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryPlan {
    pub node: Option<TopLevelPlanNode>,
    pub statistics: QueryPlanningStatistics,
}

#[derive(Debug, PartialEq, derive_more::From, Serialize, Deserialize)]
pub enum TopLevelPlanNode {
    Subscription(SubscriptionNode),
    #[from(FetchNode, Box<FetchNode>)]
    Fetch(Box<FetchNode>),
    Sequence(SequenceNode),
    Parallel(ParallelNode),
    Flatten(FlattenNode),
    Defer(DeferNode),
    #[from(ConditionNode, Box<ConditionNode>)]
    Condition(Box<ConditionNode>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionNode {
    pub primary: Box<FetchNode>,
    // XXX(@goto-bus-stop) Is this not just always a SequenceNode?
    pub rest: Option<Box<PlanNode>>,
}

#[derive(Debug, Clone, PartialEq, derive_more::From, Serialize, Deserialize)]
pub enum PlanNode {
    #[from(FetchNode, Box<FetchNode>)]
    Fetch(Box<FetchNode>),
    Sequence(SequenceNode),
    Parallel(ParallelNode),
    Flatten(FlattenNode),
    Defer(DeferNode),
    #[from(ConditionNode, Box<ConditionNode>)]
    Condition(Box<ConditionNode>),
}

impl From<PlanNode> for TopLevelPlanNode {
    fn from(node: PlanNode) -> Self {
        match node {
            PlanNode::Fetch(fetch_node) => TopLevelPlanNode::Fetch(fetch_node),
            PlanNode::Sequence(sequence_node) => TopLevelPlanNode::Sequence(sequence_node),
            PlanNode::Parallel(parallel_node) => TopLevelPlanNode::Parallel(parallel_node),
            PlanNode::Flatten(flatten_node) => TopLevelPlanNode::Flatten(flatten_node),
            PlanNode::Defer(defer_node) => TopLevelPlanNode::Defer(defer_node),
            PlanNode::Condition(condition_node) => TopLevelPlanNode::Condition(condition_node),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchNode {
    pub subgraph_name: Arc<str>,
    /// Optional identifier for the fetch for defer support. All fetches of a given plan will be
    /// guaranteed to have a unique `id`.
    pub id: Option<u64>,
    pub variable_usages: Vec<Name>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub requires: Vec<requires_selection::Selection>,
    // PORT_NOTE: We don't serialize the "operation" string in this struct, as these query plan
    // nodes are meant for direct consumption by router (without any serdes), so we leave the
    // question of whether it needs to be serialized to router.
    pub operation_document: serializable_document::SerializableDocument,
    pub operation_name: Option<Name>,
    #[serde(with = "crate::utils::serde_bridge::operation_type")]
    pub operation_kind: executable::OperationType,
    /// Optionally describe a number of "rewrites" that query plan executors should apply to the
    /// data that is sent as the input of this fetch. Note that such rewrites should only impact the
    /// inputs of the fetch they are applied to (meaning that, as those inputs are collected from
    /// the current in-memory result, the rewrite should _not_ impact said in-memory results, only
    /// what is sent in the fetch).
    pub input_rewrites: Arc<Vec<Arc<FetchDataRewrite>>>,
    /// Similar to `input_rewrites`, but for optional "rewrites" to apply to the data that is
    /// received from a fetch (and before it is applied to the current in-memory results).
    pub output_rewrites: Vec<Arc<FetchDataRewrite>>,
    /// Similar to the other kinds of rewrites. This is a mechanism to convert a contextual path into
    /// an argument to a resolver. Note value setters are currently unused here, but may be used in
    /// the future.
    pub context_rewrites: Vec<Arc<FetchDataRewrite>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceNode {
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelNode {
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlattenNode {
    pub path: Vec<FetchDataPathElement>,
    pub node: Box<PlanNode>,
}

/// A `DeferNode` corresponds to one or more `@defer` applications at the same level of "nestedness"
/// in the planned query.
///
/// It contains a "primary block" and a vector of "deferred blocks". The "primary block" represents
/// the part of the query that is _not_ deferred (so the part of the query up until we reach the
/// @defer(s) this handles), while each "deferred block" correspond to the deferred part of one of
/// the @defer(s) handled by the node.
///
/// Note that `DeferNode`s are only generated if defer support is enabled for the query planner.
/// Also note that if said support is enabled, then `DeferNode`s are always generated if the query
/// has a @defer application, even if in some cases generated plan may not "truly" defer the
/// underlying fetches (i.e. in cases where `deferred[*].node` are all undefined). This currently
/// happens because some specific cases of defer cannot be handled, but could later also happen if
/// we implement more advanced server-side heuristics to decide if deferring is judicious or not.
/// This allows the executor of the plan to consistently send a defer-abiding multipart response to
/// the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferNode {
    /// The "primary" part of a defer, that is the non-deferred part (though could be deferred
    /// itself for a nested defer).
    pub primary: PrimaryDeferBlock,
    /// The "deferred" parts of the defer (note that it's a vector). Each of those deferred elements
    /// will correspond to a different chunk of the response to the client (after the initial
    /// on-deferred one that is).
    pub deferred: Vec<DeferredDeferBlock>,
}

/// The primary block of a `DeferNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrimaryDeferBlock {
    /// The part of the original query that "selects" the data to send in that primary response
    /// once the plan in `node` completes). Note that if the parent `DeferNode` is nested, then it
    /// must come inside the `DeferredNode` in which it is nested, and in that case this
    /// sub-selection will start at that parent `DeferredNode.query_path`. Note that this can be
    /// `None` in the rare case that everything in the original query is deferred (which is not very
    /// useful  in practice, but not disallowed by the @defer spec at the moment).
    pub sub_selection: Option<String>,
    /// The plan to get all the data for the primary block. Same notes as for subselection: usually
    /// defined, but can be undefined in some corner cases where nothing is to be done in the
    /// primary block.
    pub node: Option<Box<PlanNode>>,
}

/// A deferred block of a `DeferNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredDeferBlock {
    /// References one or more fetch node(s) (by `id`) within `DeferNode.primary.node`. The plan of
    /// this deferred part should not be started until all such fetches return.
    pub depends: Vec<DeferredDependency>,
    /// The optional defer label.
    pub label: Option<String>,
    /// Path, in the query, to the `@defer` application this corresponds to. The `sub_selection`
    /// starts at this `query_path`.
    pub query_path: Vec<QueryPathElement>,
    /// The part of the original query that "selects" the data to send in the deferred response
    /// (once the plan in `node` completes). Will be set _unless_ `node` is a `DeferNode` itself.
    pub sub_selection: Option<String>,
    /// The plan to get all the data for this deferred block. Usually set, but can be `None` for a
    /// `@defer` application where everything has been fetched in the "primary block" (i.e. when
    /// this deferred block only exists to expose what should be send to the upstream client in a
    /// deferred response), but without declaring additional fetches. This happens for @defer
    /// applications that cannot be handled through the query planner and where the defer cannot be
    /// passed through to the subgraph).
    pub node: Option<Box<PlanNode>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredDependency {
    /// A `FetchNode` ID.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionNode {
    pub condition_variable: Name,
    pub if_clause: Option<Box<PlanNode>>,
    pub else_clause: Option<Box<PlanNode>>,
}

/// The type of rewrites currently supported on the input/output data of fetches.
///
/// A rewrite usually identifies some sub-part of the data and some action to perform on that
/// sub-part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, derive_more::From)]
pub enum FetchDataRewrite {
    ValueSetter(FetchDataValueSetter),
    KeyRenamer(FetchDataKeyRenamer),
}

/// A rewrite that sets a value at the provided path of the data it is applied to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchDataValueSetter {
    /// Path to the value that is set by this "rewrite".
    pub path: Vec<FetchDataPathElement>,
    /// The value to set at `path`. Note that the query planner currently only uses string values,
    /// but that may change in the future.
    pub set_value_to: serde_json_bytes::Value,
}

/// A rewrite that renames the key at the provided path of the data it is applied to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchDataKeyRenamer {
    /// Path to the key that is renamed by this "rewrite".
    pub path: Vec<FetchDataPathElement>,
    /// The key to rename to at `path`.
    pub rename_key_to: Name,
}

/// Vectors of this element match path(s) to a value in fetch data. Each element is (1) a key in
/// object data, (2) _any_ index in array data (often serialized as `@`), (3) a typename constraint
/// on the object data at that point in the path(s) (a path should only match for objects whose
/// `__typename` is the provided type), or (4) a parent indicator to move upwards one level in the
/// object.
///
/// It's possible for vectors of this element to match no paths in fetch data, e.g. if an object key
/// doesn't exist, or if an object's `__typename` doesn't equal the provided one. If this occurs,
/// then query plan execution should not execute the instruction this path is associated with.
///
/// The path starts at the top of the data it is applied to. So for instance, for fetch data inputs,
/// the path starts at the root of the object representing those inputs.
///
/// Note that the `@` is currently optional in some contexts, as query plan execution may assume
/// upon encountering array data in a path that it should match the remaining path to the array's
/// elements.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FetchDataPathElement {
    Key(Name, Option<Conditions>),
    AnyIndex(Option<Conditions>),
    TypenameEquals(Name),
    Parent,
}

pub type Conditions = Vec<Name>;

/// Vectors of this element match a path in a query. Each element is (1) a field in a query, or (2)
/// an inline fragment in a query.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
pub enum QueryPathElement {
    Field { response_key: Name },
    InlineFragment { type_condition: Name },
}

impl PlanNode {
    /// Returns the kind of plan node this is as a human-readable string. Exact output not guaranteed.
    fn node_kind(&self) -> &'static str {
        match self {
            Self::Fetch(_) => "Fetch",
            Self::Sequence(_) => "Sequence",
            Self::Parallel(_) => "Parallel",
            Self::Flatten(_) => "Flatten",
            Self::Defer(_) => "Defer",
            Self::Condition(_) => "Condition",
        }
    }
}

pub(crate) mod entity_finder {
    use apollo_compiler::collections::IndexSet;

    use super::*;
    use crate::bail;
    use crate::ensure;
    use crate::error::FederationError;
    use crate::internal_error;
    use crate::supergraph::FEDERATION_ENTITIES_FIELD_NAME;

    pub fn reduce_query_plan_for_entity_finder(
        query_plan: QueryPlan,
        target_paths: &IndexSet<Vec<QueryPathElement>>,
    ) -> Result<(QueryPlan, Vec<EntityFilter>), FederationError> {
        let Some(root_node) = &query_plan.node else {
            return Ok((
                QueryPlan {
                    node: query_plan.node,
                    statistics: query_plan.statistics,
                },
                Vec::new(),
            ));
        };
        let (node, filters) = visit_top_level_plan_node(target_paths, root_node)?;
        Ok((
            QueryPlan {
                node,
                statistics: query_plan.statistics,
            },
            filters,
        ))
    }

    fn visit_plan_node(
        target_paths: &IndexSet<Vec<QueryPathElement>>,
        current_path: &[FetchDataPathElement],
        node: &PlanNode,
    ) -> Result<(Option<PlanNode>, Vec<EntityFilter>), FederationError> {
        match node {
            PlanNode::Fetch(fetch_node) => visit_fetch_node(target_paths, current_path, fetch_node),
            PlanNode::Sequence(sequence_node) => {
                visit_sequence_node(target_paths, current_path, sequence_node)
            }
            PlanNode::Parallel(parallel_node) => {
                visit_parallel_node(target_paths, current_path, parallel_node)
            }
            PlanNode::Flatten(flatten_node) => {
                visit_plan_node(target_paths, &flatten_node.path, &flatten_node.node)
            }
            _ => todo!(),
        }
    }

    fn lift_to_top_level(
        value: Result<(Option<PlanNode>, Vec<EntityFilter>), FederationError>,
    ) -> Result<(Option<TopLevelPlanNode>, Vec<EntityFilter>), FederationError> {
        value.map(|(node, filters)| (node.map(|n| n.into()), filters))
    }

    fn visit_top_level_plan_node(
        target_paths: &IndexSet<Vec<QueryPathElement>>,
        node: &TopLevelPlanNode,
    ) -> Result<(Option<TopLevelPlanNode>, Vec<EntityFilter>), FederationError> {
        match node {
            TopLevelPlanNode::Fetch(fetch_node) => {
                lift_to_top_level(visit_fetch_node(target_paths, &[], fetch_node))
            }
            TopLevelPlanNode::Flatten(flatten_node) => lift_to_top_level(visit_plan_node(
                target_paths,
                &flatten_node.path,
                &flatten_node.node,
            )),
            TopLevelPlanNode::Sequence(sequence_node) => {
                lift_to_top_level(visit_sequence_node(target_paths, &[], sequence_node))
            }
            TopLevelPlanNode::Parallel(parallel_node) => {
                lift_to_top_level(visit_parallel_node(target_paths, &[], parallel_node))
            }
            _ => todo!(),
        }
    }

    fn visit_sequence_node(
        target_paths: &IndexSet<Vec<QueryPathElement>>,
        current_path: &[FetchDataPathElement],
        sequence_node: &SequenceNode,
    ) -> Result<(Option<PlanNode>, Vec<EntityFilter>), FederationError> {
        let mut nodes = Vec::new();
        let mut all_filters = Vec::new();
        for sub_node in &sequence_node.nodes {
            let (plan, filters) = visit_plan_node(target_paths, current_path, sub_node)?;
            if let Some(plan) = plan {
                nodes.push(plan);
            }
            all_filters.extend(filters);
        }
        Ok((
            if nodes.is_empty() {
                None
            } else {
                Some(PlanNode::Sequence(SequenceNode { nodes }))
            },
            all_filters,
        ))
    }

    fn visit_parallel_node(
        target_paths: &IndexSet<Vec<QueryPathElement>>,
        current_path: &[FetchDataPathElement],
        parallel_node: &ParallelNode,
    ) -> Result<(Option<PlanNode>, Vec<EntityFilter>), FederationError> {
        let mut nodes = Vec::new();
        let mut all_filters = Vec::new();
        for sub_node in &parallel_node.nodes {
            let (plan, filters) = visit_plan_node(target_paths, current_path, sub_node)?;
            if let Some(plan) = plan {
                nodes.push(plan);
            }
            all_filters.extend(filters);
        }
        Ok((
            if nodes.is_empty() {
                None
            } else {
                Some(PlanNode::Parallel(ParallelNode { nodes }))
            },
            all_filters,
        ))
    }

    pub struct EntityFilter {
        pub response_path: Vec<FetchDataPathElement>,
        pub subgraph_name: Arc<str>,
        pub entity_key_fields: requires_selection::InlineFragment,
    }

    fn visit_fetch_node(
        target_paths: &IndexSet<Vec<QueryPathElement>>,
        current_path: &[FetchDataPathElement],
        fetch_node: &FetchNode,
    ) -> Result<(Option<PlanNode>, Vec<EntityFilter>), FederationError> {
        let filter = visit_fetch_node_inner(target_paths, current_path, fetch_node)?;
        if let Some(entity_filter) = filter {
            Ok((None, vec![entity_filter]))
        } else {
            Ok((Some(PlanNode::Fetch(Box::new(fetch_node.clone()))), vec![]))
        }
    }

    fn visit_fetch_node_inner(
        target_paths: &IndexSet<Vec<QueryPathElement>>,
        current_path: &[FetchDataPathElement],
        fetch_node: &FetchNode,
    ) -> Result<Option<EntityFilter>, FederationError> {
        if fetch_node.requires.is_empty() {
            // root fetch node
            return Ok(None);
        }

        let doc = fetch_node
            .operation_document
            .as_parsed()
            .map_err(|e| internal_error!("{e}"))?;
        let Ok(query) = doc.operations.get(None) else {
            bail!("Expected an operation in the fetch node, but found none");
        };
        let clipped_target_paths = clip_target_paths(target_paths, current_path);
        // Entity fetch query has a set of inline fragments.
        let Some((first, rest)) = query.selection_set.selections.split_first() else {
            bail!("Expected at least one selection in the fetch query");
        };
        let executable::Selection::Field(entities_field) = first else {
            bail!(
                "Expected the first selection in the fetch query to be the `{FEDERATION_ENTITIES_FIELD_NAME}` field, but found: {first}"
            );
        };
        ensure!(
            rest.is_empty(),
            "Expected the fetch query to have only the `{FEDERATION_ENTITIES_FIELD_NAME}` field, but found more selections: {rest:?}"
        );
        for entity_selection in &entities_field.selection_set.selections {
            let executable::Selection::InlineFragment(entity_fragment) = entity_selection else {
                bail!("Selection is unexpectedly not an inline fragment.");
            };
            let Some(entity_type) = &entity_fragment.type_condition else {
                bail!("Type condition is unexpectedly missing.");
            };
            for target_path in &clipped_target_paths {
                if !selection_set_contains_path(&entity_fragment.selection_set, target_path) {
                    continue;
                }
                let Some(requires) = find_representation(&fetch_node.requires, entity_type) else {
                    bail!("Requires selection not found for entity type: {entity_type}");
                };
                return Ok(Some(EntityFilter {
                    response_path: current_path.to_vec(),
                    subgraph_name: fetch_node.subgraph_name.clone(),
                    entity_key_fields: requires.clone(),
                }));
            }
        }
        Ok(None)
    }

    // Note: This is a hack, may not work in all cases.
    fn find_representation<'a>(
        requires: &'a [requires_selection::Selection],
        entity_type: &Name,
    ) -> Option<&'a requires_selection::InlineFragment> {
        requires.iter().find_map(|selection| match selection {
            requires_selection::Selection::InlineFragment(inline) => {
                if inline.type_condition.as_ref() == Some(entity_type) {
                    Some(inline)
                } else {
                    None
                }
            }
            _ => None,
        })
    }

    fn drop_path_prefix<'a>(
        target_path: &'a [QueryPathElement],
        prefix_path: &[FetchDataPathElement],
    ) -> Option<&'a [QueryPathElement]> {
        let Some((prefix_first, prefix_rest)) = prefix_path.split_first() else {
            return Some(target_path); // empty prefix path => return the whole target path
        };
        match prefix_first {
            FetchDataPathElement::Key(response_name, _cond) => {
                // TODO: handle `cond`
                let Some((target_elem, target_rest)) = target_path.split_first() else {
                    // unexpected end of target path => resort to empty path
                    return None;
                };
                match target_elem {
                    QueryPathElement::Field { response_key } if response_key == response_name => {
                        drop_path_prefix(target_rest, prefix_rest)
                    }
                    // TODO: check inline fragments against the `cond`.
                    _ => None, // no match
                }
            }
            FetchDataPathElement::AnyIndex(_cond) => {
                // TODO: handle `cond`
                drop_path_prefix(target_path, prefix_rest)
            }
            FetchDataPathElement::TypenameEquals(_type_name) => {
                unreachable!("Unexpected TypenameEquals variant in a flatten path");
            }
            FetchDataPathElement::Parent => {
                unreachable!("Unexpected Parent variant in a flatten path");
            }
        }
    }

    fn clip_target_paths<'a>(
        target_paths: &'a IndexSet<Vec<QueryPathElement>>,
        current_path: &[FetchDataPathElement],
    ) -> Vec<&'a [QueryPathElement]> {
        target_paths
            .iter()
            .filter_map(|path| drop_path_prefix(path, current_path))
            .collect()
    }

    fn selection_set_contains_path(
        selection_set: &executable::SelectionSet,
        path: &[QueryPathElement],
    ) -> bool {
        let Some((first, rest)) = path.split_first() else {
            // Empty path means we match everything
            return true;
        };
        selection_set
            .selections
            .iter()
            .any(|selection| match (first, selection) {
                (QueryPathElement::Field { response_key }, executable::Selection::Field(field)) => {
                    response_key == field.response_key()
                        && selection_set_contains_path(&field.selection_set, rest)
                }
                (
                    QueryPathElement::InlineFragment { type_condition },
                    executable::Selection::InlineFragment(inline),
                ) => {
                    let Some(inline_type_condition) = &inline.type_condition else {
                        return selection_set_contains_path(&inline.selection_set, path);
                    };
                    type_condition == inline_type_condition
                        && selection_set_contains_path(&inline.selection_set, rest)
                }
                _ => false,
            })
    }

    // for debugging
    #[allow(dead_code)]
    fn query_path_to_string(path: &[QueryPathElement]) -> String {
        path.iter()
            .map(|elem| elem.to_string())
            .collect::<Vec<_>>()
            .join("::")
    }

    // for debugging
    #[allow(dead_code)]
    fn fetch_data_path_to_string(path: &[FetchDataPathElement]) -> String {
        path.iter()
            .map(|elem| elem.to_string())
            .collect::<Vec<_>>()
            .join("::")
    }
}
