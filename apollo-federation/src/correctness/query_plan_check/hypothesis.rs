//! What a configuration can be, read from what was fetched before: the hypothesis every
//! obligation is decided under.
//!
//! A copy's runtime type is admissible where some contribution under its key at its parent is
//! active and every active one's result holds the type: the model's `responseKeyConfigurations`,
//! read from the view of what was fetched before the obligation, *available*. Where no type is
//! admissible the copy is absent, and nothing below it is owed.
//!
//! An obligation states that every configuration in a region, its demand, lies in the union of
//! some clauses, its supplies, among the admissible configurations in which its copy is present.
//! It is decided over its own variables: the hypothesis is projected onto the variables the demand
//! and supplies mention, by eliminating the others along the copy's chain from the bottom up, each
//! variable after its last use, and the supplies are subtracted from what remains. The state the
//! elimination carries at a copy is the existential projection of the constraints of the copy and
//! its ancestors onto the variables the obligation still needs; it is memoized by copy and those
//! variables, within one view. No obligation's result is cached, and supports belonging to
//! different obligations are never combined into one region: obligations over separate variables
//! never multiply.
//!
//! A context obligation reads copies off the fetch's own chain, on branches the rewrites descend.
//! Those copies are variables of their own, each of which may be absent; their constraints are
//! eliminated with the chain's projection, deepest first.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::region::Algebra;
use super::region::Clause;
use super::region::Literal;
use super::region::Region;
use super::region::TypeSet;
use super::region::Value;
use super::region::Var;
use super::region::everything;
use super::region::vars;
use super::response::CopyId;
use super::response::View;

/// How many of the copies from an obligation's own up an explanation names the runtime types of.
const EXPLAINED_ANCESTORS: usize = 3;

/// A copy's admissible runtime types, each set with the region of its ancestors' configurations it
/// is admissible in.
type Groups = Rc<Vec<(TypeSet, Region)>>;

/// The projections of copies' chains, by copy and the variables kept.
type Projections = HashMap<(CopyId, Vec<Var>), Rc<Region>>;

/// The admissible configurations of the copies a view holds.
pub(super) struct Hypothesis<'h> {
    pub(super) algebra: &'h Algebra,
    pub(super) view: View<'h>,
    domains: RefCell<HashMap<CopyId, TypeSet>>,
    groups: RefCell<HashMap<CopyId, Groups>>,
    projections: RefCell<Projections>,
}

impl<'h> Hypothesis<'h> {
    pub(super) fn new(algebra: &'h Algebra, view: View<'h>) -> Self {
        Hypothesis {
            algebra,
            view,
            domains: Default::default(),
            groups: Default::default(),
            projections: Default::default(),
        }
    }

    /// The runtime types a copy's variable can take: every supergraph object some contribution
    /// under its key declares, or absent. The root's is its own type.
    pub(super) fn domain(&self, copy: CopyId) -> TypeSet {
        if let Some(domain) = self.domains.borrow().get(&copy) {
            return *domain;
        }
        let algebra = self.algebra;
        let response = self.view.response;
        let domain = match response.parent(copy) {
            None => algebra.objects([response.root_type()]),
            Some((parent, key)) => self
                .view
                .fields_at(parent, key)
                .filter_map(|field| field.result)
                .fold(algebra.absent(), |all, result| algebra.union(all, result)),
        };
        self.domains.borrow_mut().insert(copy, domain);
        domain
    }

    /// The supergraph objects a copy can be, absent excluded.
    pub(super) fn object_domain(&self, copy: CopyId) -> TypeSet {
        self.algebra.minus(self.domain(copy), self.algebra.absent())
    }

    /// The runtime types a region allows a copy, among those the copy can have.
    pub(super) fn allowed(&self, copy: CopyId, region: &Region) -> TypeSet {
        let algebra = self.algebra;
        let domain = self.object_domain(copy);
        let allowed = region.iter().fold(TypeSet::EMPTY, |all, clause| {
            let own = clause
                .iter()
                .find(|literal| literal.var == Var::Copy(copy))
                .and_then(|literal| match literal.value {
                    Value::Types(types) => Some(types),
                    Value::Is(_) => None,
                })
                .unwrap_or(domain);
            algebra.union(all, own)
        });
        algebra.intersect(allowed, domain)
    }

    /// How literals are completed in this view: against the variable's domain.
    pub(super) fn completion(&self) -> impl Fn(&Var) -> TypeSet + '_ {
        let objects = self.algebra.every_object();
        move |var: &Var| match var {
            Var::Copy(copy) => self.domain(*copy),
            _ => objects,
        }
    }

    /// A copy's admissible runtime types, grouped by the region of the configurations of its
    /// ancestors in which each is admissible: some contribution under its key is active and none
    /// whose result excludes the type is.
    fn groups(&self, copy: CopyId) -> Groups {
        if let Some(groups) = self.groups.borrow().get(&copy) {
            return groups.clone();
        }
        let algebra = self.algebra;
        let domain = self.completion();
        algebra.work.admissibility.add(1);
        let Some((parent, key)) = self.view.response.parent(copy) else {
            return Rc::new(vec![(self.domain(copy), everything())]);
        };
        let fields: Vec<(&Region, Vec<u32>)> = self
            .view
            .fields_at(parent, key)
            .map(|field| {
                let bits = field
                    .result
                    .map(|result| algebra.bits(result))
                    .unwrap_or_default();
                (&field.guard, bits)
            })
            .collect();
        let mut signatures: Vec<(Vec<bool>, Vec<u32>)> = Vec::new();
        for bit in algebra.bits(self.object_domain(copy)) {
            algebra.work.admissibility_reads.add(fields.len() as u64);
            let signature: Vec<bool> = fields
                .iter()
                .map(|(_, bits)| bits.binary_search(&bit).is_ok())
                .collect();
            match signatures.iter_mut().find(|(known, _)| *known == signature) {
                Some((_, bits)) => bits.push(bit),
                None => signatures.push((signature, vec![bit])),
            }
        }
        let mut groups: Vec<(TypeSet, Region)> = Vec::new();
        for (signature, bits) in signatures {
            let including: Region = fields
                .iter()
                .zip(&signature)
                .filter(|(_, holds)| **holds)
                .flat_map(|((guard, _), _)| guard.iter().cloned())
                .collect();
            let excluding: Vec<Clause> = algebra.simplify(
                fields
                    .iter()
                    .zip(&signature)
                    .filter(|(_, holds)| !**holds)
                    .flat_map(|((guard, _), _)| guard.iter().cloned())
                    .collect(),
                &domain,
            );
            let including = algebra.simplify(including, &domain);
            let admissible = algebra.simplify(
                algebra.subtract_all(including, &excluding, &domain),
                &domain,
            );
            if admissible.is_empty() {
                continue;
            }
            let types = algebra.of_bits(bits);
            match groups.iter_mut().find(|(_, known)| *known == admissible) {
                Some((known, _)) => *known = algebra.union(*known, types),
                None => groups.push((types, admissible)),
            }
        }
        algebra.work.admissibility_groups.add(groups.len() as u64);
        let groups = Rc::new(groups);
        self.groups.borrow_mut().insert(copy, groups.clone());
        groups
    }

    /// The configurations of a copy's ancestors in which it is present.
    fn present(&self, copy: CopyId) -> Region {
        let groups = self.groups(copy);
        let domain = self.completion();
        self.algebra.simplify(
            groups
                .iter()
                .flat_map(|(_, admissible)| admissible.iter().cloned())
                .collect(),
            &domain,
        )
    }

    /// A copy's constraint: its variable takes an admissible type, or, where `absent` allows it
    /// and no type is admissible, none.
    fn constraint(&self, copy: CopyId, absent: bool) -> Region {
        let algebra = self.algebra;
        let domain = self.completion();
        let mut region: Region = Vec::new();
        for (types, admissible) in self.groups(copy).iter() {
            let own = [Literal {
                var: Var::Copy(copy),
                value: Value::Types(*types),
            }];
            region.extend(
                admissible
                    .iter()
                    .filter_map(|clause| algebra.conjoin(clause, &own)),
            );
        }
        if absent {
            let own = [Literal {
                var: Var::Copy(copy),
                value: Value::Types(algebra.absent()),
            }];
            let missing = algebra.subtract_all(everything(), &self.present(copy), &domain);
            region.extend(
                missing
                    .iter()
                    .filter_map(|clause| algebra.conjoin(clause, &own)),
            );
        }
        algebra.simplify(region, &domain)
    }

    /// The configurations of a copy's chain, the copy and every ancestor present, projected onto
    /// `keep`: the existential projection of their constraints, memoized by copy and `keep`.
    pub(super) fn chain(&self, copy: CopyId, mut keep: Vec<Var>) -> Rc<Region> {
        keep.sort();
        keep.dedup();
        let algebra = self.algebra;
        let Some((parent, _)) = self.view.response.parent(copy) else {
            return Rc::new(everything());
        };
        algebra.work.projection_key_width.add(keep.len() as u64);
        let key = (copy, keep);
        if let Some(projection) = self.projections.borrow().get(&key) {
            algebra.work.projection_hits.add(1);
            return projection.clone();
        }
        let (_, keep) = &key;
        algebra.work.projections.add(1);
        algebra.work.projection_support.add(keep.len() as u64);
        algebra.work.projection_support_max.max(keep.len() as u64);
        let groups = self.groups(copy);
        let own = Var::Copy(copy);
        let mut above: Vec<Var> = keep.iter().filter(|var| **var != own).cloned().collect();
        for (_, admissible) in groups.iter() {
            for clause in admissible {
                algebra.work.support_literals.add(clause.len() as u64);
                above.extend(vars(clause).cloned());
            }
        }
        let parent_projection = self.chain(parent, above);
        let kept_own = keep.contains(&own);
        let mut region: Region = Vec::new();
        for (types, admissible) in groups.iter() {
            for clause in algebra.product(admissible, &parent_projection) {
                let clause = if kept_own {
                    match algebra.conjoin(
                        &clause,
                        &[Literal {
                            var: own.clone(),
                            value: Value::Types(*types),
                        }],
                    ) {
                        Some(clause) => clause,
                        None => continue,
                    }
                } else {
                    clause
                };
                region.push(algebra.retain(clause, |var| keep.contains(var)));
            }
        }
        let domain = self.completion();
        let region = Rc::new(algebra.simplify(region, &domain));
        algebra.work.projection_clauses.add(region.len() as u64);
        self.projections.borrow_mut().insert(key, region.clone());
        region
    }

    /// The configurations of the tree of copies made of a copy's chain and `branches`, copies off
    /// the chain whose parents are on it or among them, projected onto `keep`. The chain's copies
    /// are present; a branch copy may be absent.
    pub(super) fn tree(&self, anchor: CopyId, branches: &[CopyId], keep: &[Var]) -> Region {
        let algebra = self.algebra;
        let domain = self.completion();
        let response = self.view.response;
        let chain: Vec<CopyId> = response.ancestors(anchor);
        let mut branches: Vec<CopyId> = branches
            .iter()
            .copied()
            .filter(|copy| !chain.contains(copy))
            .collect();
        branches.sort_by_key(|copy| std::cmp::Reverse(response.ancestors(*copy).len()));
        branches.dedup();
        let mut constraints: Vec<Region> = branches
            .iter()
            .map(|copy| self.constraint(*copy, true))
            .collect();
        let mut chain_keep: Vec<Var> = keep
            .iter()
            .filter(|var| match var {
                Var::Copy(copy) => chain.contains(copy),
                _ => true,
            })
            .cloned()
            .collect();
        for constraint in &constraints {
            for clause in constraint {
                for var in vars(clause) {
                    if !matches!(var, Var::Copy(copy) if branches.contains(copy)) {
                        chain_keep.push(var.clone());
                    }
                }
            }
        }
        constraints.push((*self.chain(anchor, chain_keep)).clone());
        // Bucket elimination, the branches' variables first, deepest first.
        let mut order: Vec<Var> = branches.iter().map(|copy| Var::Copy(*copy)).collect();
        for constraint in &constraints {
            for clause in constraint {
                for var in vars(clause) {
                    if !order.contains(var) {
                        order.push(var.clone());
                    }
                }
            }
        }
        for var in order {
            if keep.contains(&var) {
                continue;
            }
            let (involved, rest): (Vec<Region>, Vec<Region>) =
                constraints.into_iter().partition(|constraint| {
                    constraint
                        .iter()
                        .any(|clause| vars(clause).any(|v| *v == var))
                });
            constraints = rest;
            if involved.is_empty() {
                continue;
            }
            let mut joined = everything();
            for constraint in &involved {
                joined = algebra.simplify(algebra.product(&joined, constraint), &domain);
            }
            let joined: Region = joined
                .into_iter()
                .map(|clause| algebra.retain(clause, |other| *other != var))
                .collect();
            constraints.push(algebra.simplify(joined, &domain));
        }
        let mut region = everything();
        for constraint in &constraints {
            region = algebra.simplify(algebra.product(&region, constraint), &domain);
        }
        region
    }

    /// The clauses of the guards supplying an obligation, as few as hold the same configurations:
    /// computed once per copy and key, and subtracted most general first, so that a supply another
    /// one implies neither splits what is left nor widens the obligation's variables.
    pub(super) fn supplies(&self, guards: Vec<Clause>) -> Vec<Clause> {
        let domain = self.completion();
        self.algebra.simplify(guards, &domain)
    }

    /// Does every configuration of `demand` in which `copy` is present lie in some clause of
    /// `supplies`, as [`Self::supplies`] gives them? `None` if so; otherwise a clause of the
    /// configurations left, for the explanation.
    pub(super) fn uncovered(
        &self,
        copy: CopyId,
        demand: &Clause,
        supplies: &[Clause],
    ) -> Option<Clause> {
        let mut keep: Vec<Var> = vars(demand).cloned().collect();
        for supply in supplies {
            keep.extend(vars(supply).cloned());
        }
        self.algebra
            .work
            .support_literals
            .add((demand.len() + supplies.iter().map(Vec::len).sum::<usize>()) as u64);
        let base = self.algebra.product(
            &self.chain(copy, keep.clone()),
            std::slice::from_ref(demand),
        );
        let residual = self.residual(base, supplies)?;
        Some(self.explained(copy, keep, residual, &[]))
    }

    /// What is left of an obligation, with the runtime types of the copy and its nearest
    /// ancestors it is left at, for the explanation. The nearest alone: a projection onto every
    /// ancestor can hold as many clauses as there are paths down the chain.
    fn explained(
        &self,
        copy: CopyId,
        mut keep: Vec<Var>,
        residual: Clause,
        branches: &[CopyId],
    ) -> Clause {
        let nearest = self
            .view
            .response
            .ancestors(copy)
            .into_iter()
            .rev()
            .take(EXPLAINED_ANCESTORS)
            .filter(|copy| *copy != CopyId::ROOT)
            .map(Var::Copy);
        keep.extend(nearest);
        keep.sort();
        keep.dedup();
        let projection = if branches.is_empty() {
            (*self.chain(copy, keep)).clone()
        } else {
            self.tree(copy, branches, &keep)
        };
        self.algebra
            .product(&projection, std::slice::from_ref(&residual))
            .into_iter()
            .next()
            .unwrap_or(residual)
    }

    /// [`Self::uncovered`] over a tree of copies, for a context obligation.
    pub(super) fn uncovered_in_tree(
        &self,
        anchor: CopyId,
        branches: &[CopyId],
        demand: &Clause,
        supplies: &[Clause],
    ) -> Option<Clause> {
        let mut keep: Vec<Var> = vars(demand).cloned().collect();
        for supply in supplies {
            keep.extend(vars(supply).cloned());
        }
        self.algebra
            .work
            .support_literals
            .add((demand.len() + supplies.iter().map(Vec::len).sum::<usize>()) as u64);
        keep.sort();
        keep.dedup();
        let base = self.algebra.product(
            &self.tree(anchor, branches, &keep),
            std::slice::from_ref(demand),
        );
        let residual = self.residual(base, supplies)?;
        Some(self.explained(anchor, keep, residual, branches))
    }

    fn residual(&self, base: Region, supplies: &[Clause]) -> Option<Clause> {
        let algebra = self.algebra;
        algebra.work.obligations.add(1);
        let domain = self.completion();
        algebra
            .subtract_all(base, supplies, &domain)
            .into_iter()
            .next()
    }

    /// Is some configuration of `clause` one in which `copy` is present?
    pub(super) fn feasible(&self, copy: CopyId, clause: &Clause) -> bool {
        let base = self.algebra.product(
            &self.chain(copy, vars(clause).cloned().collect()),
            std::slice::from_ref(clause),
        );
        self.residual(base, &[]).is_some()
    }
}
