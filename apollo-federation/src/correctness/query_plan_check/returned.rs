//! What a plan's fetches can return, and the demand narrowed to it.
//!
//! Port of `QueryPlan.returnedTypes` and `QueryPlan.admits` in
//! `Apollo/Definitions/QueryPlanCompleteness.lean`. Below each field, the client is owed only the
//! object types some fetch of the plan selecting that field can return, read from each fetch's
//! operation in its subgraph's schema and pooled per object type and field over the whole plan. A
//! path through any other type can never occur in the response, where the subgraphs' declarations
//! correspond to the supergraph's, which composition establishes.
//!
//! The model narrows the demanded side into a new operation (`narrowDemand`) only because its
//! inclusion decider takes no path constraint. `query_compare` takes one, so the returned types are
//! a [`PathConstraint`] here, built once per plan, narrowing every obligation the right side of a
//! comparison opens.
//!
//! This replaces `SubgraphConstraint`, which admits what any subgraph resolving a field can return
//! rather than what the plan's fetches do. The two differ where subgraphs sharing a field disagree
//! on its types: the planner takes them to agree, and a plan asking the wider of them for the
//! field, the narrower for its subfields, is rejected here.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;

use super::ComparisonError;
use super::fetch_operation;
use super::representation::SchemaTypeLookup;
use crate::correctness::response_shape_compare::PathConstraint;
use crate::correctness::response_shape_compare::PossibleTypes;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;
use crate::schema::position::ObjectTypeDefinitionPosition;

/// The object types the plan's fetches can return for each field, keyed by the object type the
/// field is selected at and the field's name. All are supergraph names.
#[derive(Default)]
pub(super) struct ReturnedTypes {
    children_by_field: IndexMap<(Name, Name), IndexSet<Name>>,
}

impl ReturnedTypes {
    /// What every fetch of `plan` can return. A fetch naming no subgraph there is returns
    /// nothing; the walk rejects it.
    pub(super) fn of_plan(
        supergraph_schema: &ValidFederationSchema,
        subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
        plan: &QueryPlan,
    ) -> Result<Self, ComparisonError> {
        let mut returned = ReturnedTypes::default();
        let supergraph_types = SchemaTypeLookup::new(supergraph_schema);
        for fetch in fetch_nodes(plan) {
            let Some(subgraph_schema) = subgraphs_by_name.get(fetch.subgraph_name.as_ref()) else {
                continue;
            };
            let (root_type, selections) = fetch_operation(fetch, subgraph_schema)?;
            returned.collect(
                &supergraph_types,
                &SchemaTypeLookup::new(subgraph_schema),
                &mut IndexSet::default(),
                &[root_type],
                &selections,
            );
        }
        Ok(returned)
    }

    /// What one fetch's selections can return, read at the runtime types `object_types` of the
    /// subgraph's schema and named in the supergraph's. A subgraph type standing for several
    /// supergraph objects, an interface object, stands for each of them on both sides.
    ///
    /// What a field returns at an object type depends on nothing else, so `recorded` holds the
    /// `(object type, field)` pairs whose types this fetch already recorded, and each is recorded
    /// once however many positions select it.
    fn collect(
        &mut self,
        supergraph_types: &SchemaTypeLookup<'_>,
        subgraph_types: &SchemaTypeLookup<'_>,
        recorded: &mut IndexSet<(Name, Name)>,
        object_types: &[Name],
        selections: &[Selection],
    ) {
        for selection in selections {
            match selection {
                Selection::Field(field) => {
                    let mut reached: Vec<Name> = Vec::new();
                    for object_type in object_types {
                        let result_types =
                            subgraph_types.field_result_types(object_type, &field.name);
                        if recorded.insert((object_type.clone(), field.name.clone())) {
                            let parent_types = supergraph_types.possible_types(object_type);
                            for result_type in result_types.iter() {
                                let child_types = supergraph_types.possible_types(result_type);
                                for parent_type in parent_types.iter() {
                                    self.children_by_field
                                        .entry((parent_type.clone(), field.name.clone()))
                                        .or_default()
                                        .extend(child_types.iter().cloned());
                                }
                            }
                        }
                        for result_type in result_types.iter() {
                            if !reached.contains(result_type) {
                                reached.push(result_type.clone());
                            }
                        }
                    }
                    self.collect(
                        supergraph_types,
                        subgraph_types,
                        recorded,
                        &reached,
                        &field.selection_set.selections,
                    );
                }
                Selection::InlineFragment(fragment) => {
                    let applying: Vec<Name> = object_types
                        .iter()
                        .filter(|object_type| {
                            fragment.type_condition.as_ref().is_none_or(|condition| {
                                subgraph_types
                                    .possible_types(condition)
                                    .contains(object_type)
                            })
                        })
                        .cloned()
                        .collect();
                    self.collect(
                        supergraph_types,
                        subgraph_types,
                        recorded,
                        &applying,
                        &fragment.selection_set.selections,
                    );
                }
                // Spreads are inlined before a fetch's operation is read.
                Selection::FragmentSpread(_) => {}
            }
        }
    }

    /// The object types a field, selected at `parent_type`, is returned as.
    pub(super) fn children(
        &self,
        parent_type: &Name,
        field_name: &Name,
    ) -> Option<&IndexSet<Name>> {
        self.children_by_field
            .get(&(parent_type.clone(), field_name.clone()))
    }

    /// Does some fetch return a `child_type` for the field, selected at `parent_type`?
    pub(super) fn returns(&self, parent_type: &Name, field_name: &Name, child_type: &Name) -> bool {
        self.children(parent_type, field_name)
            .is_some_and(|children| children.contains(child_type))
    }

    /// The returned types as a path constraint.
    pub(super) fn constraint(&self) -> ReturnedConstraint<'_> {
        ReturnedConstraint(self)
    }
}

/// Every fetch node of a plan, a subscription's primary fetch included.
fn fetch_nodes(plan: &QueryPlan) -> Vec<&FetchNode> {
    fn walk<'a>(node: &'a PlanNode, out: &mut Vec<&'a FetchNode>) {
        match node {
            PlanNode::Fetch(fetch) => out.push(fetch),
            PlanNode::Sequence(sequence) => sequence.nodes.iter().for_each(|n| walk(n, out)),
            PlanNode::Parallel(parallel) => parallel.nodes.iter().for_each(|n| walk(n, out)),
            PlanNode::Flatten(flatten) => walk(&flatten.node, out),
            PlanNode::Defer(defer) => {
                defer.primary.node.iter().for_each(|n| walk(n, out));
                defer
                    .deferred
                    .iter()
                    .filter_map(|block| block.node.as_ref())
                    .for_each(|n| walk(n, out));
            }
            PlanNode::Condition(node) => {
                node.if_clause.iter().for_each(|n| walk(n, out));
                node.else_clause.iter().for_each(|n| walk(n, out));
            }
        }
    }

    let mut out = Vec::new();
    match &plan.node {
        None => {}
        Some(TopLevelPlanNode::Subscription(subscription)) => {
            out.push(&*subscription.primary);
            subscription.rest.iter().for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Fetch(fetch)) => out.push(fetch),
        Some(TopLevelPlanNode::Sequence(node)) => node.nodes.iter().for_each(|n| walk(n, &mut out)),
        Some(TopLevelPlanNode::Parallel(node)) => node.nodes.iter().for_each(|n| walk(n, &mut out)),
        Some(TopLevelPlanNode::Flatten(node)) => walk(&node.node, &mut out),
        Some(TopLevelPlanNode::Defer(node)) => {
            node.primary.node.iter().for_each(|n| walk(n, &mut out));
            node.deferred
                .iter()
                .filter_map(|block| block.node.as_ref())
                .for_each(|n| walk(n, &mut out));
        }
        Some(TopLevelPlanNode::Condition(node)) => {
            node.if_clause.iter().for_each(|n| walk(n, &mut out));
            node.else_clause.iter().for_each(|n| walk(n, &mut out));
        }
    }
    out
}

/// [`ReturnedTypes`] as a [`PathConstraint`]: a field's obligation is narrowed to the object types
/// some fetch returns for it at its parent type. Stateless, so any two narrow alike.
#[derive(Clone, Copy)]
pub(super) struct ReturnedConstraint<'a>(&'a ReturnedTypes);

impl PathConstraint for ReturnedConstraint<'_> {
    fn for_field(
        &self,
        representative_field: &Field,
        parent_types: &PossibleTypes,
    ) -> Result<(Self, PossibleTypes), ComparisonError> {
        let PossibleTypes::Restricted(parent_types) = parent_types else {
            return Ok((*self, PossibleTypes::All));
        };
        let mut allowed = IndexSet::default();
        for parent_type in parent_types {
            if let Some(children) = self
                .0
                .children(&parent_type.type_name, &representative_field.name)
            {
                allowed.extend(
                    children
                        .iter()
                        .map(|child| ObjectTypeDefinitionPosition::new(child.clone())),
                );
            }
        }
        Ok((*self, PossibleTypes::Restricted(allowed)))
    }

    fn narrows_alike(&self, _other: &Self) -> bool {
        true
    }
}
