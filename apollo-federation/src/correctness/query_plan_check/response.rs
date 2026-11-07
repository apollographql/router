//! The response a plan has fetched: the copies fetches contribute to, and the contributions.
//!
//! Port of `Apollo/Definitions/QueryPlanResponse.lean`, with each copy's possible runtime types
//! kept as a variable of the configurations rather than one copy per runtime type.
//!
//! # Copies and contributions
//!
//! A *copy* is the value one response key holds at one parent copy, the root being the response's
//! own: the model's receiving occurrence, a [`CopyId`] interned by parent copy and final response
//! key. Every fetch writing that key at that parent writes the same value — a later entity fetch
//! whose result `inverted_paths` restores there, a sibling in a sequence, parallel root fetches at
//! the root — and the router's `type_aware_deep_merge` merges what they write, objects field by
//! field and lists index by index; a list's elements are one copy standing for every index.
//! Different parents and different keys are different copies, so nothing fetched under one alias,
//! one list or one parent reaches another.
//!
//! A *contribution* is one field application one fetch made on one copy, read at one subgraph
//! parent type: its producer, the subgraph type its fields were read at, the key it is read under,
//! the field and arguments, its guard and its result. There is no first producer: no contribution
//! decides what the copy is for another.
//!
//! # Runtime types
//!
//! What a copy is in a configuration is the value of its variable (`region.rs`). A contribution's
//! *guard* is the region of configurations where the plan made it: the Boolean literals it was
//! fetched under, the runtime type of its own copy that its subgraph parent type stands for
//! through the declaration correspondence (see the checker's module docs), the type conditions of
//! the path its fetch ran at, and the fragments of the fetch's own operation that selected it where
//! they apply to some of a copy's subgraph parent types and not others. Its *result* is the set of
//! supergraph objects its subgraph declares for the field at that parent type, with the sharing
//! clause's restriction (`sharing.rs`): `None` for a missing declaration or an unjustified
//! correspondence, and the empty set for a field with no possible object result types, which is
//! not a scalar leaf and not `None`.
//!
//! The copy a key holds is admissible as a runtime type exactly when some contribution to the key
//! is active and every active one's result holds that type; where none is, there is no copy and
//! nothing is owed below, though the containing field is (`hypothesis.rs`). Different parents'
//! runtime types stay correlated with their parents through the guards, never multiplied out.
//!
//! # Building a fetch's contributions
//!
//! A fetch's additions are built once per syntactic selection: a field selected on a copy with
//! several subgraph parent types records one contribution per parent type and produces its
//! subselections once, on the one child, over the union of the results. They are built on a tree
//! of the fetch's own, under the keys it selects; its output renames are applied to that tree in
//! order, and only then is each node mounted on the copy its parent copy and final key name.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;

use super::super::query_compare::conditions::literals_for_directives;
use super::super::query_compare::schema_view::SchemaView;
use super::ComparisonError;
use super::region::Algebra;
use super::region::Clause;
use super::region::Literal;
use super::region::Region;
use super::region::TypeSet;
use super::region::Value;
use super::region::Var;
use super::region::boolean;
use super::region::everything;
use super::sharing::Permitted;
use super::sharing::Sharing;
use super::subgraph::Subgraph;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;

//==================================================================================================
// Copies and contributions
//==================================================================================================

/// A producing fetch, numbered in the order the walk reaches fetches: the walk reaches every fetch
/// node exactly once, so that order is the model's structural address.
pub(super) type ProducerId = u32;

/// A copy: the value one response key holds at one parent copy. [`CopyId::ROOT`] is the
/// response's root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct CopyId(u32);

impl CopyId {
    pub(super) const ROOT: CopyId = CopyId(0);
}

struct CopyNode {
    /// The parent copy and the key it holds this copy under; `None` at the root.
    parent: Option<(CopyId, Name)>,
}

/// One field application one fetch made on one copy: the model's `ProducedField`.
pub(super) struct Contribution {
    pub(super) copy: CopyId,
    /// The key it is read under, after its fetch's output renames.
    pub(super) key: Name,
    pub(super) producer: ProducerId,
    pub(super) subgraph_parent: Name,
    pub(super) field: Name,
    pub(super) arguments: Vec<Node<ast::Argument>>,
    pub(super) guard: Region,
    /// The supergraph objects its result can be: `None` for a missing declaration or an
    /// unjustified correspondence.
    pub(super) result: Option<TypeSet>,
}

impl Contribution {
    /// Is this the copy's own `__typename` value, the one the router's `iterate_path` reads? An
    /// alias, or another field under the key, is not.
    pub(super) fn is_typename(&self) -> bool {
        self.key == *INTROSPECTION_TYPENAME_FIELD_NAME
            && self.field == *INTROSPECTION_TYPENAME_FIELD_NAME
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct ContributionId(u32);

/// Everything the plan has fetched, in one arena.
pub(super) struct Response {
    root_type: Name,
    copies: Vec<CopyNode>,
    interned: HashMap<(CopyId, Name), CopyId>,
    contributions: Vec<Contribution>,
    /// Every contribution, by the copy it is on and the key it is read under.
    by_key: HashMap<(CopyId, Name), Vec<ContributionId>>,
    producers: Vec<Arc<str>>,
}

impl Response {
    pub(super) fn new(root_type: Name) -> Self {
        Response {
            root_type,
            copies: vec![CopyNode { parent: None }],
            interned: HashMap::new(),
            contributions: Vec::new(),
            by_key: HashMap::new(),
            producers: Vec::new(),
        }
    }

    pub(super) fn root_type(&self) -> &Name {
        &self.root_type
    }

    pub(super) fn new_producer(&mut self, subgraph_name: Arc<str>) -> ProducerId {
        self.producers.push(subgraph_name);
        (self.producers.len() - 1) as ProducerId
    }

    pub(super) fn producer(&self, producer: ProducerId) -> &str {
        &self.producers[producer as usize]
    }

    pub(super) fn contribution(&self, id: ContributionId) -> &Contribution {
        &self.contributions[id.0 as usize]
    }

    pub(super) fn parent(&self, copy: CopyId) -> Option<(CopyId, &Name)> {
        self.copies[copy.0 as usize]
            .parent
            .as_ref()
            .map(|(parent, key)| (*parent, key))
    }

    /// The copy a key holds at a parent copy, if any fetch has written it.
    pub(super) fn child(&self, copy: CopyId, key: &Name) -> Option<CopyId> {
        self.interned.get(&(copy, key.clone())).copied()
    }

    /// Every copy from the root down to this one, the model's `ResponsePosition.ancestors`.
    pub(super) fn ancestors(&self, copy: CopyId) -> Vec<CopyId> {
        let mut ancestors = vec![copy];
        let mut current = copy;
        while let Some((parent, _)) = self.parent(current) {
            ancestors.push(parent);
            current = parent;
        }
        ancestors.reverse();
        ancestors
    }

    fn intern(&mut self, algebra: &Algebra, parent: CopyId, key: &Name) -> CopyId {
        algebra.work.copy_probes.add(1);
        if let Some(copy) = self.interned.get(&(parent, key.clone())) {
            return *copy;
        }
        let copy = CopyId(self.copies.len() as u32);
        self.copies.push(CopyNode {
            parent: Some((parent, key.clone())),
        });
        self.interned.insert((parent, key.clone()), copy);
        algebra.work.copies.add(1);
        copy
    }

    /// A copy as an explanation names it: the keys leading to it.
    pub(super) fn describe(&self, copy: CopyId) -> String {
        if copy == CopyId::ROOT {
            return format!("the root `{}`", self.root_type);
        }
        let keys: Vec<String> = self.ancestors(copy)[1..]
            .iter()
            .filter_map(|copy| self.parent(*copy).map(|(_, key)| key.to_string()))
            .collect();
        format!("`{}`", keys.join("."))
    }

    /// A literal on a copy's runtime type. The root's is its own type, so a literal on it holds or
    /// fails outright.
    pub(super) fn type_literal(
        &self,
        algebra: &Algebra,
        copy: CopyId,
        types: TypeSet,
    ) -> Option<Clause> {
        if copy == CopyId::ROOT {
            return algebra.contains(types, &self.root_type).then(Vec::new);
        }
        (types != TypeSet::EMPTY).then(|| {
            vec![Literal {
                var: Var::Copy(copy),
                value: Value::Types(types),
            }]
        })
    }

    pub(super) fn view<'r>(&'r self, ids: &'r [ContributionId]) -> View<'r> {
        View {
            response: self,
            ids,
        }
    }
}

/// Two sorted, duplicate-free lists of contribution ids as one.
pub(super) fn union(left: &[ContributionId], right: &[ContributionId]) -> Vec<ContributionId> {
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => {
                merged.push(left[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                merged.push(right[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                merged.push(left[i]);
                i += 1;
                j += 1;
            }
        }
    }
    merged.extend_from_slice(&left[i..]);
    merged.extend_from_slice(&right[j..]);
    merged
}

/// Stored guards complete a literal against every object: a contribution is never on an absent
/// copy.
pub(super) fn every_object(algebra: &Algebra) -> impl Fn(&Var) -> TypeSet + '_ {
    let objects = algebra.every_object();
    move |_| objects
}

//==================================================================================================
// Reading a view
//==================================================================================================

/// What a fetch has before it, or can read: some of the response's contributions, by sorted id.
#[derive(Clone, Copy)]
pub(super) struct View<'r> {
    pub(super) response: &'r Response,
    ids: &'r [ContributionId],
}

impl<'r> View<'r> {
    fn contains(&self, id: ContributionId) -> bool {
        self.ids.binary_search(&id).is_ok()
    }

    /// The contributions in the view at a copy under a key: the model's `responseFields`, before
    /// the configuration is applied.
    pub(super) fn fields_at(
        &self,
        copy: CopyId,
        key: &Name,
    ) -> impl Iterator<Item = &'r Contribution> + '_ {
        self.response
            .by_key
            .get(&(copy, key.clone()))
            .into_iter()
            .flatten()
            .filter(|id| self.contains(**id))
            .map(|id| self.response.contribution(*id))
    }

    /// The guards under which a copy's own `__typename` value was fetched: the model's
    /// `responseTypenameConditions`.
    pub(super) fn typename_guards(&self, copy: CopyId) -> Vec<Clause> {
        self.fields_at(copy, &INTROSPECTION_TYPENAME_FIELD_NAME)
            .filter(|field| field.is_typename())
            .flat_map(|field| field.guard.iter().cloned())
            .collect()
    }

    /// The configurations in which the view reaches a path's copies: at each step, some
    /// contribution under the key is active. That the step's child has one of that contribution's
    /// result types is left to admissibility, which every reader of a guard assumes, and which
    /// holds of this view's contributions in every later view, since each holds this one.
    pub(super) fn reach(&self, algebra: &Algebra, steps: &[(CopyId, Name)]) -> Region {
        let domain = every_object(algebra);
        let mut region = everything();
        for (copy, key) in steps {
            algebra.work.path_steps.add(1);
            let guards: Region = self
                .fields_at(*copy, key)
                .inspect(|_| algebra.work.path_contributions.add(1))
                .flat_map(|field| field.guard.iter().cloned())
                .collect();
            let guards = algebra.simplify(guards, &domain);
            region = algebra.simplify(algebra.product(&region, &guards), &domain);
            if region.is_empty() {
                break;
            }
        }
        region
    }
}

//==================================================================================================
// Where a fetch runs
//==================================================================================================

/// The copy a flatten path addresses, with what it is addressed under: the model's
/// `ReachedPosition`, of which a path of keys and indices has one. `region` holds the path's type
/// conditions, each read literally against the runtime type of the copy it is written at, and
/// `reads` the copies whose `__typename` values those conditions read, recorded rather than
/// required here, so that a `__typename` value that was not fetched does not erase the position it
/// is owed at. `steps` are the keys followed, by the copy each was read at.
#[derive(Clone)]
pub(super) struct Position {
    pub(super) copy: CopyId,
    pub(super) region: Clause,
    pub(super) reads: Vec<CopyId>,
    pub(super) steps: Vec<(CopyId, Name)>,
}

impl Response {
    /// The copy a flatten path addresses: the model's `responsePositions`. A path holding anything
    /// but keys and indices addresses none, and neither does one whose conditions no runtime type
    /// meets, or whose keys no fetch has written.
    pub(super) fn position(
        &self,
        algebra: &Algebra,
        path: &[FetchDataPathElement],
    ) -> Option<Position> {
        let mut position = Position {
            copy: CopyId::ROOT,
            region: Vec::new(),
            reads: Vec::new(),
            steps: Vec::new(),
        };
        for element in path {
            algebra.work.path_steps.add(1);
            let condition = match element {
                FetchDataPathElement::Key(name, condition) => {
                    let child = self.child(position.copy, name)?;
                    position.steps.push((position.copy, name.clone()));
                    position.copy = child;
                    condition
                }
                FetchDataPathElement::AnyIndex(condition) => condition,
                FetchDataPathElement::TypenameEquals(_) | FetchDataPathElement::Parent => {
                    return None;
                }
            };
            if let Some(names) = condition {
                let literal = self.type_literal(algebra, position.copy, algebra.objects(names))?;
                position.region = algebra.conjoin(&position.region, &literal)?;
                position.reads.push(position.copy);
            }
        }
        Some(position)
    }
}

//==================================================================================================
// Producing a fetch's contributions
//==================================================================================================

/// What one fetch's contributions are read with: its subgraph and the other providers of each
/// field it selects, the supergraph its results correspond to, and its producer identity.
pub(super) struct Production<'p> {
    pub(super) subgraph: &'p Subgraph<'p>,
    pub(super) subgraph_name: &'p str,
    pub(super) sharing: &'p Sharing<'p>,
    pub(super) supergraph: &'p SchemaView<'p>,
    pub(super) algebra: &'p Algebra,
    pub(super) producer: ProducerId,
}

/// Where a fetch's operation is read: the copy it runs on, the region it runs there under, and
/// the subgraph types its root selections are read at, each with the supergraph objects it stands
/// for there.
pub(super) struct Mount {
    pub(super) copy: CopyId,
    pub(super) base: Region,
    pub(super) cases: Vec<(Name, TypeSet)>,
}

/// A field's result cases at one parent type, each a subgraph object and the supergraph object it
/// stands for: `None` for a missing declaration or an unjustified correspondence.
type ResultCases = Option<Vec<(Name, Name)>>;

/// A node of a fetch's own tree: node 0 is the copy it runs on.
struct LocalNode {
    parent: Option<u32>,
    key: Name,
}

/// A contribution on the fetch's own tree, before it is mounted.
#[derive(Clone)]
struct LocalContribution {
    node: u32,
    key: Name,
    /// The node its subselections were produced on, shared by the selection's contributions.
    child: Option<u32>,
    subgraph_parent: Name,
    field: Name,
    arguments: Vec<Node<ast::Argument>>,
    guard: Region,
    result: Option<TypeSet>,
}

struct Local<'p> {
    production: &'p Production<'p>,
    mount: CopyId,
    nodes: Vec<LocalNode>,
    contributions: Vec<LocalContribution>,
}

impl Local<'_> {
    fn var(&self, node: u32) -> Var {
        if node == 0 {
            Var::Copy(self.mount)
        } else {
            Var::Local(node)
        }
    }

    /// A literal on a node's runtime type; none on the root, whose type is fixed.
    fn type_literal(&self, node: u32, types: TypeSet) -> Clause {
        if node == 0 && self.mount == CopyId::ROOT {
            return Vec::new();
        }
        vec![Literal {
            var: self.var(node),
            value: Value::Types(types),
        }]
    }

    fn conjoin_into(&self, region: &[Clause], clause: &Clause) -> Region {
        let algebra = self.production.algebra;
        let domain = every_object(algebra);
        algebra.simplify(
            algebra.product(region, std::slice::from_ref(clause)),
            &domain,
        )
    }

    /// The contributions a fetch's selections make on one node, read at each of its subgraph
    /// parent types: the model's `producedSelections`, once per selection rather than once per
    /// parent type.
    ///
    /// Fragments are read in the fetch's subgraph at each parent type; one applying to some of
    /// them and not others narrows the node's runtime type for what it selects. A field's result
    /// is its declaration's, read before its subselections, so selecting fewer fragments below it
    /// narrows nothing, and a field with no possible object result types still stands.
    fn produce(
        &mut self,
        node: u32,
        cases: &[(Name, TypeSet)],
        inherited: &Region,
        selections: &[Selection],
    ) -> Result<(), ComparisonError> {
        let production = self.production;
        let algebra = production.algebra;
        for selection in selections {
            algebra.work.selections_visited.add(1);
            match selection {
                Selection::Field(field) => {
                    let Some(literals) = literals_for_directives(&field.directives) else {
                        continue;
                    };
                    let Some(directives) = algebra.clause(literals.iter().map(boolean)) else {
                        continue;
                    };
                    let guard = self.conjoin_into(inherited, &directives);
                    if guard.is_empty() {
                        continue;
                    }
                    let key = field.response_name().clone();
                    let child = (!field.selection_set.selections.is_empty()).then(|| {
                        self.nodes.push(LocalNode {
                            parent: Some(node),
                            key: key.clone(),
                        });
                        (self.nodes.len() - 1) as u32
                    });
                    let mut below: Vec<(Name, Vec<Name>)> = Vec::new();
                    for (subgraph_parent, objects) in cases {
                        for (types, results) in self.results(subgraph_parent, *objects, field)? {
                            let result = results.as_ref().map(|results| {
                                algebra.objects(results.iter().map(|(_, object)| object))
                            });
                            for (subgraph_type, object) in results.iter().flatten() {
                                match below.iter_mut().find(|(known, _)| known == subgraph_type) {
                                    Some((_, objects)) => {
                                        if !objects.contains(object) {
                                            objects.push(object.clone());
                                        }
                                    }
                                    None => {
                                        below.push((subgraph_type.clone(), vec![object.clone()]))
                                    }
                                }
                            }
                            algebra.work.contributions_recorded.add(1);
                            let own = self.type_literal(node, types);
                            self.contributions.push(LocalContribution {
                                node,
                                key: key.clone(),
                                child,
                                subgraph_parent: subgraph_parent.clone(),
                                field: field.name.clone(),
                                arguments: field.arguments.clone(),
                                guard: self.conjoin_into(&guard, &own),
                                result,
                            });
                        }
                    }
                    if let Some(child) = child
                        && !below.is_empty()
                    {
                        let below: Vec<(Name, TypeSet)> = below
                            .iter()
                            .map(|(subgraph_type, objects)| {
                                (subgraph_type.clone(), algebra.objects(objects))
                            })
                            .collect();
                        self.produce(child, &below, &guard, &field.selection_set.selections)?;
                    }
                }
                Selection::InlineFragment(fragment) => {
                    let applicable: Vec<(Name, TypeSet)> = cases
                        .iter()
                        .filter(|(subgraph_parent, _)| {
                            fragment
                                .type_condition
                                .as_ref()
                                .is_none_or(|type_condition| {
                                    production
                                        .subgraph
                                        .type_includes_object(type_condition, subgraph_parent)
                                })
                        })
                        .cloned()
                        .collect();
                    if applicable.is_empty() {
                        continue;
                    }
                    let Some(literals) = literals_for_directives(&fragment.directives) else {
                        continue;
                    };
                    let Some(mut narrowing) = algebra.clause(literals.iter().map(boolean)) else {
                        continue;
                    };
                    if applicable.len() < cases.len() {
                        let types = applicable.iter().fold(TypeSet::EMPTY, |all, (_, objects)| {
                            algebra.union(all, *objects)
                        });
                        let Some(narrowed) =
                            algebra.conjoin(&narrowing, &self.type_literal(node, types))
                        else {
                            continue;
                        };
                        narrowing = narrowed;
                    }
                    let guard = self.conjoin_into(inherited, &narrowing);
                    if guard.is_empty() {
                        continue;
                    }
                    self.produce(
                        node,
                        &applicable,
                        &guard,
                        &fragment.selection_set.selections,
                    )?;
                }
                // Spreads are inlined before a fetch's selections are produced.
                Selection::FragmentSpread(_) => {}
            }
        }
        Ok(())
    }

    /// A field's result cases at one subgraph parent type, grouped by the supergraph objects the
    /// parent type stands for that the sharing clause restricts alike: the model's
    /// `Subgraphs.sharedFieldCases`. `__typename` has no object result types and no providers.
    fn results(
        &self,
        subgraph_parent: &Name,
        objects: TypeSet,
        field: &Field,
    ) -> Result<Vec<(TypeSet, ResultCases)>, ComparisonError> {
        let production = self.production;
        let algebra = production.algebra;
        if field.name == *INTROSPECTION_TYPENAME_FIELD_NAME {
            return Ok(vec![(objects, Some(Vec::new()))]);
        }
        let declared = production.subgraph.field_runtime_types(
            &algebra.work,
            production.supergraph,
            subgraph_parent,
            &field.name,
        );
        let mut groups: Vec<(TypeSet, ResultCases)> = Vec::new();
        for parent in algebra.names(objects) {
            let permitted = production.sharing.permitted(
                algebra,
                production.supergraph,
                production.subgraph_name,
                &parent,
                &field.name,
            )?;
            let results = declared.as_ref().and_then(|declared| match permitted {
                Permitted::Anything => Some(declared.clone()),
                Permitted::Only(permitted) => Some(
                    declared
                        .iter()
                        .filter(|(_, object)| algebra.contains(permitted, object))
                        .cloned()
                        .collect(),
                ),
                Permitted::Unjustified => None,
            });
            let parent = algebra.objects([&parent]);
            match groups.iter_mut().find(|(_, known)| *known == results) {
                Some((types, _)) => *types = algebra.union(*types, parent),
                None => groups.push((parent, results)),
            }
        }
        Ok(groups)
    }

    /// The nodes a rewrite path reaches before the key it renames, each with the region of the
    /// configurations it reaches it in: the model's `rewritePositions`. A rewrite path carries no
    /// flatten-path condition, so an index's is not read; a `TypenameEquals` narrows the node it
    /// is written at.
    fn rewrite_positions(&self, path: &[FetchDataPathElement]) -> Vec<(u32, Region)> {
        let algebra = self.production.algebra;
        let mut reached = vec![(0, everything())];
        for element in path {
            reached = match element {
                FetchDataPathElement::Key(name, _) => {
                    let mut next: Vec<(u32, Region)> = Vec::new();
                    for (node, region) in &reached {
                        for contribution in &self.contributions {
                            let Some(child) = contribution.child else {
                                continue;
                            };
                            if contribution.node != *node || contribution.key != *name {
                                continue;
                            }
                            match next.iter_mut().find(|(known, _)| *known == child) {
                                Some((_, known)) => {
                                    for clause in region {
                                        if !known.contains(clause) {
                                            known.push(clause.clone());
                                        }
                                    }
                                }
                                None => next.push((child, region.clone())),
                            }
                        }
                    }
                    next
                }
                FetchDataPathElement::AnyIndex(_) => reached,
                FetchDataPathElement::TypenameEquals(type_name) => {
                    let types = algebra.objects([type_name]);
                    reached
                        .into_iter()
                        .filter_map(|(node, region)| {
                            if types == TypeSet::EMPTY {
                                return None;
                            }
                            let region =
                                self.conjoin_into(&region, &self.type_literal(node, types));
                            (!region.is_empty()).then_some((node, region))
                        })
                        .collect()
                }
                FetchDataPathElement::Parent => Vec::new(),
            };
        }
        reached
    }

    /// Renames a key on the fetch's own tree: the model's `renameResponse`. At each node the
    /// path reaches, the contributions under the old key move to the new one in the region the
    /// path reaches the node in, with what was produced below them; where that region is not all
    /// of a contribution's guard, the contribution is split and its subtree copied, the part
    /// outside the region keeping the old key.
    fn rename(&mut self, path: &[FetchDataPathElement], new_key: &Name) {
        let Some((FetchDataPathElement::Key(old_key, _), prefix)) = path.split_last() else {
            return;
        };
        let algebra = self.production.algebra;
        let domain = every_object(algebra);
        for (node, region) in self.rewrite_positions(prefix) {
            let outside_region = algebra.simplify(
                algebra.subtract_all(everything(), &region, &domain),
                &domain,
            );
            let at: Vec<usize> = (0..self.contributions.len())
                .filter(|index| {
                    let contribution = &self.contributions[*index];
                    contribution.node == node && contribution.key == *old_key
                })
                .collect();
            let mut children: Vec<Option<u32>> = Vec::new();
            for index in &at {
                let child = self.contributions[*index].child;
                if !children.contains(&child) {
                    children.push(child);
                }
            }
            for child in children {
                let group: Vec<usize> = at
                    .iter()
                    .copied()
                    .filter(|index| self.contributions[*index].child == child)
                    .collect();
                let parts: Vec<(Region, Region)> = group
                    .iter()
                    .map(|index| {
                        let guard = &self.contributions[*index].guard;
                        (
                            self.restricted(guard, &region),
                            algebra.simplify(
                                algebra.subtract_all(guard.clone(), &region, &domain),
                                &domain,
                            ),
                        )
                    })
                    .collect();
                if parts.iter().all(|(inside, _)| inside.is_empty()) {
                    continue;
                }
                if parts.iter().all(|(_, outside)| outside.is_empty()) {
                    for index in &group {
                        self.contributions[*index].key = new_key.clone();
                    }
                    if let Some(child) = child {
                        self.nodes[child as usize].key = new_key.clone();
                    }
                    continue;
                }
                let moved_child = child.map(|child| {
                    let copy = self.copy_subtree(child, node, new_key, &region);
                    self.restrict_subtree(child, &outside_region);
                    copy
                });
                for (index, (inside, outside)) in group.iter().zip(parts) {
                    if !inside.is_empty() {
                        algebra.work.rename_records.add(1);
                        let mut moved = self.contributions[*index].clone();
                        moved.key = new_key.clone();
                        moved.child = moved_child;
                        moved.guard = inside;
                        self.contributions.push(moved);
                    }
                    self.contributions[*index].guard = outside;
                }
            }
        }
        self.contributions
            .retain(|contribution| !contribution.guard.is_empty());
    }

    /// Copies the subtree under `node` to a new node under `parent` and `key`, and returns the new
    /// node. Every node of the subtree gets a copy, every contribution on one is copied onto its
    /// copy, and every literal on one of its nodes moves to that node's copy, so what is selected
    /// below the moved field stays correlated with the copies it moved to; literals on the nodes
    /// above stay where they are, and every guard is narrowed to `region`.
    fn copy_subtree(&mut self, node: u32, parent: u32, key: &Name, region: &Region) -> u32 {
        let algebra = self.production.algebra;
        let root = self.nodes.len() as u32;
        self.nodes.push(LocalNode {
            parent: Some(parent),
            key: key.clone(),
        });
        let mut copies: HashMap<u32, u32> = HashMap::from([(node, root)]);
        let mut order = vec![node];
        let mut at = 0;
        while let Some(current) = order.get(at).copied() {
            at += 1;
            let mut children: Vec<u32> = Vec::new();
            for contribution in &self.contributions {
                if contribution.node == current
                    && let Some(child) = contribution.child
                    && !children.contains(&child)
                {
                    children.push(child);
                }
            }
            for child in children {
                if copies.contains_key(&child) {
                    continue;
                }
                let copy = self.nodes.len() as u32;
                self.nodes.push(LocalNode {
                    parent: Some(copies[&current]),
                    key: self.nodes[child as usize].key.clone(),
                });
                copies.insert(child, copy);
                order.push(child);
            }
        }
        let below: Vec<usize> = (0..self.contributions.len())
            .filter(|index| copies.contains_key(&self.contributions[*index].node))
            .collect();
        for index in below {
            let mut contribution = self.contributions[index].clone();
            contribution.node = copies[&contribution.node];
            contribution.child = contribution.child.map(|child| copies[&child]);
            let moved: Region = contribution
                .guard
                .iter()
                .filter_map(|clause| {
                    algebra.clause(clause.iter().map(|literal| Literal {
                        var: match &literal.var {
                            Var::Local(node) => {
                                Var::Local(copies.get(node).copied().unwrap_or(*node))
                            }
                            var => var.clone(),
                        },
                        value: literal.value,
                    }))
                })
                .collect();
            contribution.guard = self.restricted(&moved, region);
            algebra.work.rename_records.add(1);
            self.contributions.push(contribution);
        }
        root
    }

    /// Narrows every guard in the subtree under `node` to `region`.
    fn restrict_subtree(&mut self, node: u32, region: &Region) {
        let below: Vec<usize> = (0..self.contributions.len())
            .filter(|index| self.contributions[*index].node == node)
            .collect();
        let mut children = Vec::new();
        for index in below {
            let guard = self.restricted(&self.contributions[index].guard, region);
            self.contributions[index].guard = guard;
            if let Some(child) = self.contributions[index].child
                && !children.contains(&child)
            {
                children.push(child);
            }
        }
        for child in children {
            self.restrict_subtree(child, region);
        }
    }

    fn restricted(&self, guard: &Region, region: &Region) -> Region {
        let algebra = self.production.algebra;
        let domain = every_object(algebra);
        algebra.simplify(algebra.product(guard, region), &domain)
    }
}

/// What the model identifies one fetch's contribution by: its copy, key, subgraph parent type,
/// field and arguments, and guard.
type Identity = (CopyId, Name, Name, Name, Vec<Node<ast::Argument>>, Region);

impl Response {
    /// What one fetch adds: its selections produced on the copy it runs on, its output renames
    /// applied to them in order, and each node mounted on the copy its parent copy and final key
    /// name. The model's `FetchNode.producedResponse` and `rewriteResponse`.
    ///
    /// A contribution identical to one the fetch has already made, in everything the model
    /// identifies one by, is kept once.
    pub(super) fn produce_fetch(
        &mut self,
        production: &Production<'_>,
        mount: Mount,
        selections: &[Selection],
        rewrites: &[Arc<FetchDataRewrite>],
    ) -> Result<Vec<ContributionId>, ComparisonError> {
        let algebra = production.algebra;
        let mut local = Local {
            production,
            mount: mount.copy,
            nodes: vec![LocalNode {
                parent: None,
                key: INTROSPECTION_TYPENAME_FIELD_NAME.clone(),
            }],
            contributions: Vec::new(),
        };
        local.produce(0, &mount.cases, &mount.base, selections)?;
        for rewrite in rewrites {
            if let FetchDataRewrite::KeyRenamer(renamer) = rewrite.as_ref() {
                local.rename(&renamer.path, &renamer.rename_key_to);
            }
        }

        let mut copies = vec![mount.copy];
        for node in &local.nodes[1..] {
            let parent = copies[node.parent.unwrap_or(0) as usize];
            copies.push(self.intern(algebra, parent, &node.key));
        }
        let domain = every_object(algebra);
        let mut seen: HashSet<Identity> = HashSet::new();
        let mut added = Vec::new();
        for contribution in local.contributions {
            let copy = copies[contribution.node as usize];
            let guard: Region = contribution
                .guard
                .iter()
                .filter_map(|clause| {
                    algebra.clause(clause.iter().map(|literal| Literal {
                        var: match &literal.var {
                            Var::Local(node) => Var::Copy(copies[*node as usize]),
                            var => var.clone(),
                        },
                        value: literal.value,
                    }))
                })
                .collect();
            let guard = algebra.simplify(guard, &domain);
            if guard.is_empty() {
                continue;
            }
            if !seen.insert((
                copy,
                contribution.key.clone(),
                contribution.subgraph_parent.clone(),
                contribution.field.clone(),
                contribution.arguments.clone(),
                guard.clone(),
            )) {
                algebra.work.contributions_deduplicated.add(1);
                continue;
            }
            if contribution
                .result
                .is_some_and(|result| result != TypeSet::EMPTY)
            {
                self.intern(algebra, copy, &contribution.key);
            }
            algebra.work.contributions.add(1);
            algebra
                .work
                .guard_literals
                .add(guard.iter().map(|clause| clause.len() as u64).sum());
            let id = ContributionId(self.contributions.len() as u32);
            self.by_key
                .entry((copy, contribution.key.clone()))
                .or_default()
                .push(id);
            self.contributions.push(Contribution {
                copy,
                key: contribution.key,
                producer: production.producer,
                subgraph_parent: contribution.subgraph_parent,
                field: contribution.field,
                arguments: contribution.arguments,
                guard,
                result: contribution.result,
            });
            added.push(id);
        }
        Ok(added)
    }

    /// What a fetch in a `defer` node's primary block sends the deferred blocks naming it: its own
    /// additions, with the contributions on their ancestor copies that lead to them and none of
    /// those copies' other values. The model's `withResponseAncestors`.
    ///
    /// A contribution is sent where its child, with one of its result types, is a prefix of a
    /// position an addition was made at, keeping its own guard, Boolean literals included. An
    /// addition's positions are the runtime types of its chain at which it is reached: every step
    /// through a field whose result holds the step's type, the fields its fetch had before it above
    /// the copy it ran on and its own below, active together and with the addition. Its guard
    /// leaves those result correlations to admissibility, which decides predicates but not
    /// prefixes, so they are joined here with the Boolean literals intact: once from the additions
    /// up, each step's variable eliminated once it is joined, and once from the root down, each
    /// copy keeping the variables of the steps and additions below it, so that every step holds
    /// the positions of the whole chain. Only the prefixes a contribution is selected by are read
    /// without Boolean literals; where they are less than all of its guard, what is sent is a
    /// narrowed copy of it.
    pub(super) fn sent(
        &mut self,
        algebra: &Algebra,
        available: &[ContributionId],
        added: &[ContributionId],
    ) -> Vec<ContributionId> {
        let domain = every_object(algebra);
        let own: HashSet<(CopyId, Name)> = added
            .iter()
            .map(|id| {
                let addition = self.contribution(*id);
                (addition.copy, addition.key.clone())
            })
            .collect();
        let copy_vars = |region: &Region| -> Vec<CopyId> {
            region
                .iter()
                .flat_map(|clause| clause.iter())
                .filter_map(|literal| match literal.var {
                    Var::Copy(copy) => Some(copy),
                    _ => None,
                })
                .collect()
        };
        // Per copy on the way up from an addition: the additions' positions at it and below it,
        // over its own variable, its ancestors' and the Boolean ones; and the ancestors the
        // additions and the steps below it read.
        let mut below: HashMap<CopyId, Region> = HashMap::new();
        let mut reads: HashMap<CopyId, HashSet<CopyId>> = HashMap::new();
        let mut copies: Vec<(usize, CopyId)> = Vec::new();
        let mut seen: HashSet<CopyId> = HashSet::new();
        for id in added {
            let addition = self.contribution(*id);
            below
                .entry(addition.copy)
                .or_default()
                .extend(addition.guard.iter().cloned());
            reads
                .entry(addition.copy)
                .or_default()
                .extend(copy_vars(&addition.guard));
            let ancestors = self.ancestors(addition.copy);
            for (depth, copy) in ancestors.into_iter().enumerate().skip(1) {
                if seen.insert(copy) {
                    copies.push((depth, copy));
                }
            }
        }
        // The step to each copy: its fetch's own fields below the copy it ran on, the fields it
        // had before it above, each under its guard and holding one of its result types.
        copies.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        let mut steps: HashMap<CopyId, Region> = HashMap::new();
        for (_, copy) in &copies {
            let copy = *copy;
            let Some((parent, key)) = self.parent(copy) else {
                continue;
            };
            let parent_key = (parent, key.clone());
            let view = self.view(if own.contains(&parent_key) {
                added
            } else {
                available
            });
            let step: Region = view
                .fields_at(parent, &parent_key.1)
                .filter_map(|field| {
                    let result = field.result.filter(|result| *result != TypeSet::EMPTY)?;
                    Some(algebra.product(
                        &field.guard,
                        &[vec![Literal {
                            var: Var::Copy(copy),
                            value: Value::Types(result),
                        }]],
                    ))
                })
                .flatten()
                .collect();
            let step = algebra.simplify(step, &domain);
            let here = algebra.simplify(below.remove(&copy).unwrap_or_default(), &domain);
            let up: Region = algebra
                .product(&step, &here)
                .into_iter()
                .map(|clause| algebra.retain(clause, |var| *var != Var::Copy(copy)))
                .collect();
            let up = algebra.simplify(up, &domain);
            let read = reads.remove(&copy).unwrap_or_default();
            reads.entry(parent).or_default().extend(
                read.iter()
                    .copied()
                    .chain(copy_vars(&step))
                    .filter(|var| *var != copy),
            );
            reads.insert(copy, read);
            below.entry(parent).or_default().extend(up);
            below.insert(copy, here);
            steps.insert(copy, step);
        }
        // From the root down: the positions at each copy, its step joined with the chain above it
        // and the additions below it.
        let mut above: HashMap<CopyId, Region> = HashMap::new();
        let mut positions: HashMap<CopyId, Region> = HashMap::new();
        for (_, copy) in copies.iter().rev() {
            let copy = *copy;
            let (Some((parent, _)), Some(step), Some(here)) =
                (self.parent(copy), steps.get(&copy), below.get(&copy))
            else {
                continue;
            };
            let chain = match above.get(&parent) {
                Some(chain) => algebra.product(chain, step),
                None => step.clone(),
            };
            let read = &reads[&copy];
            let chain: Region = chain
                .into_iter()
                .map(|clause| {
                    algebra.retain(clause, |var| match var {
                        Var::Copy(other) => {
                            *other == copy || *other == parent || read.contains(other)
                        }
                        _ => true,
                    })
                })
                .collect();
            let chain = algebra.simplify(chain, &domain);
            positions.insert(
                copy,
                algebra.simplify(algebra.product(&chain, here), &domain),
            );
            above.insert(copy, chain);
        }
        let mut sent: Vec<ContributionId> = Vec::new();
        let mut narrowed: Vec<Contribution> = Vec::new();
        for id in available {
            let field = self.contribution(*id);
            let Some(result) = field.result else {
                continue;
            };
            let Some(child) = self.child(field.copy, &field.key) else {
                continue;
            };
            let Some(at) = positions.get(&child) else {
                continue;
            };
            let leading: Region = algebra
                .product(
                    at,
                    &[vec![Literal {
                        var: Var::Copy(child),
                        value: Value::Types(result),
                    }]],
                )
                .into_iter()
                .map(|clause| {
                    algebra.retain(
                        clause,
                        |var| matches!(var, Var::Copy(copy) if *copy != child),
                    )
                })
                .collect();
            let leading = algebra.simplify(leading, &domain);
            if leading.is_empty() {
                continue;
            }
            if leading.iter().any(|clause| clause.is_empty()) {
                sent.push(*id);
                continue;
            }
            let guard = algebra.simplify(algebra.product(&field.guard, &leading), &domain);
            if guard.is_empty() {
                continue;
            }
            if guard == field.guard {
                sent.push(*id);
                continue;
            }
            narrowed.push(Contribution {
                copy: field.copy,
                key: field.key.clone(),
                producer: field.producer,
                subgraph_parent: field.subgraph_parent.clone(),
                field: field.field.clone(),
                arguments: field.arguments.clone(),
                guard,
                result: field.result,
            });
        }
        for contribution in narrowed {
            let id = ContributionId(self.contributions.len() as u32);
            self.by_key
                .entry((contribution.copy, contribution.key.clone()))
                .or_default()
                .push(id);
            self.contributions.push(contribution);
            sent.push(id);
        }
        sent.sort();
        union(&sent, added)
    }
}
