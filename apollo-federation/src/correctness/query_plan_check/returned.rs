//! What a plan's fetches can return, and the demand narrowed to it.
//!
//! Port of `QueryPlan.returnedTypes` and `QueryPlan.admits` in
//! `Apollo/Definitions/QueryPlanCompleteness.lean`. Below each field, the client is owed only the
//! object types some fetch writing that field at that position can return, read from each fetch's
//! operation in its subgraph's schema. A position is a typed key path: the response keys from the
//! query root, list indices passed over, each with the object types the object holding it can be.
//! A fetch writes at the types its fragments, its flatten path's type conditions and its entries
//! narrow each level to; a position is read at the object type of every level above it, and two
//! typed key paths name one position where their keys agree and their types meet at every level. A
//! position several fetches write admits what any of them returns. A path through any other type
//! can never occur in the response, where the subgraphs' declarations correspond to the
//! supergraph's, which composition establishes.
//!
//! An object's runtime type at a position is decided by the fetch whose response created it; an
//! entity fetch only adds fields to objects already there. So a type one subgraph can return for a
//! field at one position is not owed where another subgraph, which cannot return it, writes the
//! same field.
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
use super::ENTITIES_FIELD_NAME;
use super::fetch_operation;
use super::representation::SchemaTypeLookup;
use super::selections::apply_output_renamers;
use crate::correctness::response_shape_compare::PathConstraint;
use crate::correctness::response_shape_compare::PossibleTypes;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::schema::ValidFederationSchema;
use crate::schema::position::ObjectTypeDefinitionPosition;

/// A key path with, at each level, the object types the object holding the key can be; `None`
/// where they are not narrowed. Where a fetch writes, the types it can be writing each key at;
/// where a response is read, the types read there.
pub(super) type TypedKeyPath = Vec<(Name, Option<Vec<Name>>)>;

/// Whether a written level's object types and those a position is read at have one in common,
/// `None` admitting every one.
fn levels_meet(written: Option<&IndexSet<Name>>, read: Option<&[Name]>) -> bool {
    match (written, read) {
        (Some(written), Some(read)) => read.iter().any(|ty| written.contains(ty)),
        _ => true,
    }
}

/// A set of supergraph object types, shared where several places record the same one.
type TypeSet = Arc<IndexSet<Name>>;

/// The object types a field is selected at, and the object types it returns there.
type ReturnedPair = (TypeSet, TypeSet);

/// One typed key path some fetch writes at, a node of [`ReturnedTypes`]' trie: the levels below
/// it, and what the field it ends at returns there.
#[derive(Default)]
struct Written {
    /// The object types written at this level, `None` where not narrowed.
    level: Option<TypeSet>,
    /// The levels written below, by key: the object types written at it, and its node.
    below: IndexMap<Name, Vec<(Option<TypeSet>, usize)>>,
    /// What the field this path ends at returns, by field: the object types it is selected at and
    /// the object types it returns, each pair one subgraph object type's, so every parent in the
    /// first returns every child in the second. A parent counts only where the level holds it.
    returned: IndexMap<Name, Vec<ReturnedPair>>,
}

impl Written {
    /// The object types the field this path ends at returns, selected at `parent_type`.
    fn returned_at<'a>(
        &'a self,
        parent_type: &'a Name,
        field_name: &Name,
    ) -> impl Iterator<Item = &'a IndexSet<Name>> + 'a {
        let holds = self
            .level
            .as_ref()
            .is_none_or(|level| level.contains(parent_type));
        self.returned
            .get(field_name)
            .into_iter()
            .flatten()
            .filter(move |(parents, _)| holds && parents.contains(parent_type))
            .map(|(_, children)| &**children)
    }
}

/// The object types the plan's fetches can return for each field, at the typed key paths they
/// write it at, kept as a trie of those paths from the query root. All types are supergraph
/// names.
///
/// The model lists `(typedKeyPath, objectType, fieldName, resultType)` and asks whether one meets a
/// position (`returnedAt`). A position meets a written path where every level meets, so the
/// written paths a position can still meet are a set of the trie's nodes, narrowed one level at a
/// time; two positions reaching the same nodes are owed the same below.
pub(super) struct ReturnedTypes {
    written: Vec<Written>,
}

impl Default for ReturnedTypes {
    fn default() -> Self {
        ReturnedTypes {
            written: vec![Written::default()],
        }
    }
}

impl ReturnedTypes {
    /// What every fetch of `plan` can return, each at the key paths it writes. A fetch naming no
    /// subgraph there is returns nothing; the walk rejects it.
    pub(super) fn of_plan(
        supergraph_schema: &ValidFederationSchema,
        subgraphs_by_name: &IndexMap<Arc<str>, ValidFederationSchema>,
        plan: &QueryPlan,
    ) -> Result<Self, ComparisonError> {
        let mut returned = ReturnedTypes::default();
        let supergraph_types = SchemaTypeLookup::new(supergraph_schema);
        let mut parent_sets = IndexMap::default();
        for (path, fetch) in fetches_at(plan) {
            let Some(subgraph_schema) = subgraphs_by_name.get(fetch.subgraph_name.as_ref()) else {
                continue;
            };
            let subgraph_types = SchemaTypeLookup::new(subgraph_schema);
            // `collect` narrows the type conditions as it reads them (`narrowed_lists`).
            let (root_type, narrowed) = fetch_operation(fetch, subgraph_schema)?;
            let (typed_key_path, holders) = flatten_typed_key_path(&path);
            let start = typed_key_path.into_iter().fold(0, |node, (key, types)| {
                returned.written_at(
                    node,
                    key,
                    types.map(|types| Arc::new(IndexSet::from_iter(types))),
                )
            });
            let (holders, object_types, selections) = if fetch.requires.is_empty() {
                let supergraph_root = supergraph_schema
                    .schema()
                    .root_operation(fetch.operation_kind)
                    .cloned();
                let renamed = apply_output_renamers(
                    supergraph_schema,
                    supergraph_root.as_ref(),
                    &fetch.output_rewrites,
                    narrowed,
                );
                (holders, vec![root_type], renamed)
            } else {
                let entities_types = subgraph_types
                    .field_result_types(&root_type, &apollo_compiler::name!("_entities"))
                    .to_vec();
                let renamed = apply_output_renamers(
                    supergraph_schema,
                    None,
                    &fetch.output_rewrites,
                    entities_selections(&narrowed),
                );
                let sent = intersect_types(holders, Some(requires_types(&supergraph_types, fetch)));
                (sent, entities_types, renamed)
            };
            let mut fetch = FetchCollecting {
                supergraph_types: &supergraph_types,
                subgraph_types: &subgraph_types,
                parent_sets: &mut parent_sets,
                child_sets: IndexMap::default(),
                fields: IndexMap::default(),
                recorded: IndexSet::default(),
                lists: IndexSet::default(),
                narrowed: IndexMap::default(),
                levels: IndexMap::default(),
            };
            let object_types = fetch.lists.insert_full(object_types).0;
            returned.collect(
                &mut fetch,
                start,
                holders.map(IndexSet::from_iter).as_ref(),
                object_types,
                &[&selections],
            );
        }
        Ok(returned)
    }

    /// The node one level below `node`, written at `key` for the object types `types`.
    fn written_at(&mut self, node: usize, key: Name, types: Option<TypeSet>) -> usize {
        if let Some((_, below)) = self.written[node]
            .below
            .get(&key)
            .and_then(|written| written.iter().find(|(at_types, _)| *at_types == types))
        {
            return *below;
        }
        self.written.push(Written {
            level: types.clone(),
            ..Written::default()
        });
        let below = self.written.len() - 1;
        self.written[node]
            .below
            .entry(key)
            .or_default()
            .push((types, below));
        below
    }

    /// What one fetch's selections can return below the trie node `node`, read at the runtime
    /// types of the subgraph's schema numbered `object_types` and named in the supergraph's, the
    /// level they are read at narrowed by `holders`. Each field records the object types it is
    /// written at, its own level of the typed key path. A subgraph type standing for several
    /// supergraph objects, an interface object, stands for each of them on both sides.
    ///
    /// The fields of a level are gathered through its fragments first, by response key, field and
    /// the object types they are read at, and each group is recorded once and descended into once,
    /// with every occurrence's selections together, as field merging reads them. What is recorded
    /// is a set, so this records what visiting each occurrence would, without visiting the copies
    /// a fragment spread at several places of one level leaves.
    fn collect(
        &mut self,
        fetch: &mut FetchCollecting<'_>,
        node: usize,
        holders: Option<&IndexSet<Name>>,
        object_types: usize,
        selections: &[&[Selection]],
    ) {
        let mut groups = Groups::default();
        for selections in selections {
            fetch.gather(object_types, selections, &mut groups);
        }
        for ((response_name, field_name, object_types), below) in groups {
            let level = match holders {
                None => fetch.level(object_types),
                Some(holders) => Arc::new(
                    fetch.lists[object_types]
                        .iter()
                        .flat_map(|object_type| {
                            fetch.supergraph_types.possible_types(object_type).to_vec()
                        })
                        .filter(|ty| holders.contains(ty))
                        .collect(),
                ),
            };
            let field_node = self.written_at(node, response_name, Some(level));
            let at = fetch.field_at(object_types, &field_name);
            if fetch.recorded.insert((field_node, at)) {
                let (sets, _) = &fetch.fields[at];
                if !sets.is_empty() {
                    self.written[field_node]
                        .returned
                        .entry(field_name)
                        .or_default()
                        .extend(sets.iter().cloned());
                }
            }
            if below.iter().any(|selections| !selections.is_empty()) {
                let reached = fetch.fields[at].1;
                self.collect(fetch, field_node, None, reached, &below);
            }
        }
    }

    /// The trie node of the query root, which every position meets.
    pub(super) fn root_written(&self) -> Vec<usize> {
        vec![0]
    }

    /// The trie nodes one level below `nodes` that a position read at `key`, at the object types
    /// `types`, meets.
    pub(super) fn written_below(
        &self,
        nodes: &[usize],
        key: &Name,
        types: Option<&[Name]>,
    ) -> Vec<usize> {
        let mut out: Vec<usize> = nodes
            .iter()
            .filter_map(|node| self.written[*node].below.get(key))
            .flatten()
            .filter(|(at_types, _)| levels_meet(at_types.as_deref(), types))
            .map(|(_, below)| *below)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// What the field the trie nodes `nodes` end at returns, selected at `parent_type`.
    pub(super) fn children_at(
        &self,
        nodes: &[usize],
        parent_type: &Name,
        field_name: &Name,
    ) -> IndexSet<Name> {
        nodes
            .iter()
            .flat_map(|node| self.written[*node].returned_at(parent_type, field_name))
            .flatten()
            .cloned()
            .collect()
    }

    /// Does the field the trie nodes `nodes` end at, selected at `parent_type`, return a
    /// `child_type`?
    pub(super) fn returns_at(
        &self,
        nodes: &[usize],
        parent_type: &Name,
        field_name: &Name,
        child_type: &Name,
    ) -> bool {
        nodes.iter().any(|node| {
            self.written[*node]
                .returned_at(parent_type, field_name)
                .any(|children| children.contains(child_type))
        })
    }

    /// The object types a field at `position` (its own level last), selected at `parent_type`, is
    /// returned as: what every fetch writing it at a typed key path meeting `position` returns.
    #[cfg(test)]
    pub(super) fn children(
        &self,
        position: &[(Name, Option<Vec<Name>>)],
        parent_type: &Name,
        field_name: &Name,
    ) -> IndexSet<Name> {
        let nodes = position.iter().fold(vec![0], |nodes, (key, types)| {
            self.written_below(&nodes, key, types.as_deref())
        });
        self.children_at(&nodes, parent_type, field_name)
    }

    /// The returned types as a path constraint, starting at the query root.
    pub(super) fn constraint(&self) -> ReturnedConstraint<'_> {
        ReturnedConstraint {
            returned: self,
            nodes: vec![0],
        }
    }
}

/// A level's field occurrences by response key, field and the numbered object types they are read
/// at, with the selections below each.
type Groups<'s> = IndexMap<(Name, Name, usize), Vec<&'s [Selection]>>;

/// What `ReturnedTypes::collect` shares across one fetch's walk.
/// - Object type lists are numbered in `lists`, so occurrences group by number. `narrowed` holds
///   the lists a fragment's type condition leaves of a list, and `levels` the supergraph objects a
///   list holds.
/// - `fields` holds what a field read at a list records, the `(parents, children)` sets of each
///   object type it returns something at, and the list it reaches. It depends on nothing else, so
///   `recorded` holds the `(node, fields entry)` pairs this fetch already recorded.
/// - The sets recorded are shared: a type's possible types in `parent_sets`, across fetches, and
///   what a field of a subgraph object type returns in `child_sets`.
struct FetchCollecting<'a> {
    supergraph_types: &'a SchemaTypeLookup<'a>,
    subgraph_types: &'a SchemaTypeLookup<'a>,
    parent_sets: &'a mut IndexMap<Name, TypeSet>,
    child_sets: IndexMap<(Name, Name), TypeSet>,
    fields: IndexMap<(usize, Name), (Vec<ReturnedPair>, usize)>,
    recorded: IndexSet<(usize, usize)>,
    lists: IndexSet<Vec<Name>>,
    narrowed: IndexMap<(usize, Option<Name>), Vec<usize>>,
    levels: IndexMap<usize, TypeSet>,
}

impl FetchCollecting<'_> {
    /// Adds the field occurrences of `selections`, read at the list `object_types`, to `groups`,
    /// through fragments.
    fn gather<'s>(
        &mut self,
        object_types: usize,
        selections: &'s [Selection],
        groups: &mut Groups<'s>,
    ) {
        for selection in selections {
            match selection {
                Selection::Field(field) => groups
                    .entry((
                        field.response_name().clone(),
                        field.name.clone(),
                        object_types,
                    ))
                    .or_default()
                    .push(&field.selection_set.selections),
                Selection::InlineFragment(fragment) => {
                    let key = (object_types, fragment.type_condition.clone());
                    let at = match self.narrowed.get_index_of(&key) {
                        Some(at) => at,
                        None => {
                            let applying = narrowed_lists(
                                self.supergraph_types,
                                self.subgraph_types,
                                &self.lists[object_types],
                                fragment.type_condition.as_ref(),
                            )
                            .into_iter()
                            .map(|list| self.lists.insert_full(list).0)
                            .collect();
                            self.narrowed.insert_full(key, applying).0
                        }
                    };
                    for applying in 0..self.narrowed[at].len() {
                        let applying = self.narrowed[at][applying];
                        self.gather(applying, &fragment.selection_set.selections, groups)
                    }
                }
                // Spreads are inlined before a fetch's operation is read.
                Selection::FragmentSpread(_) => {}
            }
        }
    }

    /// The entry of `fields` for `field_name` read at the list `object_types`.
    fn field_at(&mut self, object_types: usize, field_name: &Name) -> usize {
        let key = (object_types, field_name.clone());
        if let Some(at) = self.fields.get_index_of(&key) {
            return at;
        }
        let mut sets = Vec::new();
        let mut reached: Vec<Name> = Vec::new();
        for object_type in &self.lists[object_types] {
            let result_types = self
                .subgraph_types
                .field_result_types(object_type, field_name);
            let children = self
                .child_sets
                .entry((object_type.clone(), field_name.clone()))
                .or_insert_with(|| {
                    Arc::new(
                        result_types
                            .iter()
                            .flat_map(|result_type| {
                                self.supergraph_types.possible_types(result_type).to_vec()
                            })
                            .collect(),
                    )
                })
                .clone();
            if !children.is_empty() {
                let parents = self
                    .parent_sets
                    .entry(object_type.clone())
                    .or_insert_with(|| {
                        Arc::new(
                            self.supergraph_types
                                .possible_types(object_type)
                                .iter()
                                .cloned()
                                .collect(),
                        )
                    })
                    .clone();
                sets.push((parents, children));
            }
            for result_type in result_types.iter() {
                if !reached.contains(result_type) {
                    reached.push(result_type.clone());
                }
            }
        }
        let reached = self.lists.insert_full(reached).0;
        self.fields.insert_full(key, (sets, reached)).0
    }

    /// The supergraph objects the list `object_types` holds.
    fn level(&mut self, object_types: usize) -> TypeSet {
        let supergraph_types = self.supergraph_types;
        let lists = &self.lists;
        self.levels
            .entry(object_types)
            .or_insert_with(|| {
                Arc::new(
                    lists[object_types]
                        .iter()
                        .flat_map(|object_type| {
                            supergraph_types.possible_types(object_type).to_vec()
                        })
                        .collect(),
                )
            })
            .clone()
    }
}

/// The object type lists a fragment applies to, of the object types `object_types`, as
/// `narrow_type_conditions` would rewrite it: a type condition the supergraph and subgraph agree on
/// applies to its types together, and any other to each type the subgraph admits on its own.
fn narrowed_lists(
    supergraph_types: &SchemaTypeLookup<'_>,
    subgraph_types: &SchemaTypeLookup<'_>,
    object_types: &[Name],
    type_condition: Option<&Name>,
) -> Vec<Vec<Name>> {
    let Some(condition) = type_condition else {
        return vec![object_types.to_vec()];
    };
    let admitted = subgraph_types.possible_types(condition);
    let applying = object_types
        .iter()
        .filter(|object_type| admitted.contains(object_type))
        .cloned();
    if supergraph_types
        .possible_types(condition)
        .iter()
        .all(|ty| admitted.contains(ty))
    {
        vec![applying.collect()]
    } else {
        applying.map(|object_type| vec![object_type]).collect()
    }
}

/// The typed key path of a flatten path, and the object types it narrows the object it reaches to.
/// A key's type condition narrows the value it holds, and an index's the list's elements, as
/// `selections_at` reads them.
fn flatten_typed_key_path(path: &[FetchDataPathElement]) -> (TypedKeyPath, Option<Vec<Name>>) {
    let mut typed_key_path = Vec::new();
    let mut holders: Option<Vec<Name>> = None;
    for element in path {
        match element {
            FetchDataPathElement::Key(name, condition) => {
                typed_key_path.push((name.clone(), holders));
                holders = condition.clone();
            }
            FetchDataPathElement::AnyIndex(condition) => {
                holders = intersect_types(holders, condition.clone());
            }
            FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => {}
        }
    }
    (typed_key_path, holders)
}

/// The types two narrowings admit together, `None` admitting every one.
fn intersect_types(one: Option<Vec<Name>>, other: Option<Vec<Name>>) -> Option<Vec<Name>> {
    match (one, other) {
        (None, other) => other,
        (one, None) => one,
        (Some(one), Some(other)) => Some(one.into_iter().filter(|ty| other.contains(ty)).collect()),
    }
}

/// The object types an entity fetch sends representations for: those its entries' type
/// conditions admit.
fn requires_types(supergraph_types: &SchemaTypeLookup<'_>, fetch: &FetchNode) -> Vec<Name> {
    let mut types: Vec<Name> = Vec::new();
    for entry in &fetch.requires {
        if let crate::query_plan::requires_selection::Selection::InlineFragment(fragment) = entry
            && let Some(type_condition) = &fragment.type_condition
        {
            for ty in supergraph_types.possible_types(type_condition).iter() {
                if !types.contains(ty) {
                    types.push(ty.clone());
                }
            }
        }
    }
    types
}

/// What an entity fetch's `_entities` selects; nothing for an operation of another shape, which
/// the walk rejects.
fn entities_selections(body: &[Selection]) -> Vec<Selection> {
    match body {
        [Selection::Field(field)] if field.name == ENTITIES_FIELD_NAME => {
            field.selection_set.selections.clone()
        }
        _ => Vec::new(),
    }
}

/// Every fetch node of a plan with the path it runs at: the paths of the flatten nodes above it,
/// appended in order, a subscription's primary fetch included.
fn fetches_at(plan: &QueryPlan) -> Vec<(Vec<FetchDataPathElement>, &FetchNode)> {
    fn walk<'a>(
        path: &[FetchDataPathElement],
        node: &'a PlanNode,
        out: &mut Vec<(Vec<FetchDataPathElement>, &'a FetchNode)>,
    ) {
        match node {
            PlanNode::Fetch(fetch) => out.push((path.to_vec(), fetch)),
            PlanNode::Sequence(sequence) => sequence.nodes.iter().for_each(|n| walk(path, n, out)),
            PlanNode::Parallel(parallel) => parallel.nodes.iter().for_each(|n| walk(path, n, out)),
            PlanNode::Flatten(flatten) => {
                let mut extended = path.to_vec();
                extended.extend(flatten.path.iter().cloned());
                walk(&extended, &flatten.node, out)
            }
            PlanNode::Defer(defer) => {
                defer.primary.node.iter().for_each(|n| walk(path, n, out));
                defer
                    .deferred
                    .iter()
                    .filter_map(|block| block.node.as_ref())
                    .for_each(|n| walk(path, n, out));
            }
            PlanNode::Condition(node) => {
                node.if_clause.iter().for_each(|n| walk(path, n, out));
                node.else_clause.iter().for_each(|n| walk(path, n, out));
            }
        }
    }

    let mut out = Vec::new();
    match &plan.node {
        None => {}
        Some(TopLevelPlanNode::Subscription(subscription)) => {
            out.push((Vec::new(), &*subscription.primary));
            subscription
                .rest
                .iter()
                .for_each(|n| walk(&[], n, &mut out));
        }
        Some(TopLevelPlanNode::Fetch(fetch)) => out.push((Vec::new(), fetch)),
        Some(TopLevelPlanNode::Sequence(node)) => {
            node.nodes.iter().for_each(|n| walk(&[], n, &mut out))
        }
        Some(TopLevelPlanNode::Parallel(node)) => {
            node.nodes.iter().for_each(|n| walk(&[], n, &mut out))
        }
        Some(TopLevelPlanNode::Flatten(node)) => walk(&node.path, &node.node, &mut out),
        Some(TopLevelPlanNode::Defer(node)) => {
            node.primary
                .node
                .iter()
                .for_each(|n| walk(&[], n, &mut out));
            node.deferred
                .iter()
                .filter_map(|block| block.node.as_ref())
                .for_each(|n| walk(&[], n, &mut out));
        }
        Some(TopLevelPlanNode::Condition(node)) => {
            node.if_clause.iter().for_each(|n| walk(&[], n, &mut out));
            node.else_clause.iter().for_each(|n| walk(&[], n, &mut out));
        }
    }
    out
}

/// [`ReturnedTypes`] as a [`PathConstraint`]: a field's obligation is narrowed to the object types
/// some fetch returns for it at its parent type and position. The constraint carries the trie
/// nodes the position it has descended to still meets, each level read at the parent types it was
/// read at, so two narrow alike where they meet the same written paths, whatever their types.
#[derive(Clone)]
pub(super) struct ReturnedConstraint<'a> {
    returned: &'a ReturnedTypes,
    nodes: Vec<usize>,
}

impl PathConstraint for ReturnedConstraint<'_> {
    fn for_field(
        &self,
        representative_field: &Field,
        parent_types: &PossibleTypes,
    ) -> Result<(Self, PossibleTypes), ComparisonError> {
        let level = match parent_types {
            PossibleTypes::Restricted(parent_types) => Some(
                parent_types
                    .iter()
                    .map(|parent_type| parent_type.type_name.clone())
                    .collect::<Vec<_>>(),
            ),
            PossibleTypes::All => None,
        };
        let below = ReturnedConstraint {
            returned: self.returned,
            nodes: self.returned.written_below(
                &self.nodes,
                representative_field.response_name(),
                level.as_deref(),
            ),
        };
        let Some(parent_types) = level else {
            return Ok((below, PossibleTypes::All));
        };
        let mut allowed = IndexSet::default();
        for parent_type in &parent_types {
            allowed.extend(
                self.returned
                    .children_at(&below.nodes, parent_type, &representative_field.name)
                    .into_iter()
                    .map(ObjectTypeDefinitionPosition::new),
            );
        }
        Ok((below, PossibleTypes::Restricted(allowed)))
    }

    fn narrows_alike(&self, other: &Self) -> bool {
        self.nodes == other.nodes
    }
}
