//! The sharing clause: interchangeable providers of one field application constrain its result
//! even when they are not called. The model's `Subgraphs.sharedFieldCases`.
//!
//! Sharing conformance is an execution premise beside declaration correspondence and resolver
//! conformance: the subgraphs resolving the same field at the same supergraph parent, with the same
//! arguments, return the same value, so every one of their declarations bounds what the value can
//! be, whichever subgraph a fetch asks. Apollo's resolver-consistency guidance asks this of every
//! shared field, and composition's `shareable_runtime_types::hints_for_interfaces` reads a shared
//! interface field's result as the intersection of its declarations; composition does not verify
//! that deployed resolvers agree.
//!
//! The providers of a field at a supergraph parent object are read off the subgraphs as the
//! supergraph's extraction wrote them, which is where the join declarations become subgraph
//! declarations: a field without `@join__field` is in every subgraph declaring its type, an
//! overridden field is written `@external`, and a field resolved by more than one subgraph is
//! shared, a key field implicitly. So a provider is every subgraph declaring the field at an
//! object standing for that parent through the declaration correspondence — the object of its
//! name, or an interface object of one of its interfaces — and resolving it there outright: an
//! external declaration, an overridden one among them, constrains nothing, and neither does a field
//! of the same name at an object standing for another parent. The fetch's own subgraph may resolve
//! the field only through a `@provides` window, and is then asked without being a provider.
//!
//! Which subgraph objects stand for a supergraph object is read the first time a contribution is
//! read at that object, and kept: only the object of its name and the `@interfaceObject`s of the
//! interfaces it implements can, so those alone are examined, never the subgraphs' other objects,
//! each decided from the declarations without enumerating what an interface object stands for.
//!
//! A progressive override keeps both declarations, the overriding one carrying `@override` with a
//! label: the query graph resolves the field through the one or the other under opposite
//! conditions on the label, which the plan does not fix, so the two are not providers for each
//! other. Any other subgraph resolving the field shares it with each of them, and both
//! declarations constrain its result, as every provider's does.
//!
//! What the clause adds is a restriction of the possible runtime types of the result, and nothing
//! else: no provider's declaration supplies a field, a `__typename` value or any availability.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;

use super::super::query_compare::schema_view::SchemaView;
use super::ComparisonError;
use super::region::Algebra;
use super::region::TypeSet;
use super::subgraph::Subgraph;
use crate::schema::ValidFederationSchema;
use apollo_compiler::schema::ExtendedType;

/// What the other providers of a field application permit its result to be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Permitted {
    /// No other provider resolves it outright.
    Anything,
    /// The supergraph objects every other provider's declaration admits.
    Only(TypeSet),
    /// A provider resolving it outright has a missing declaration or an unjustified
    /// correspondence for its result: an error, never an empty set.
    Unjustified,
}

/// Every subgraph, the objects standing for each supergraph object a contribution was read at, and
/// the providers of the fields read there.
pub(super) struct Sharing<'a> {
    names: Vec<Arc<str>>,
    subgraphs: Vec<Subgraph<'a>>,
    /// Per supergraph object read so far, and per subgraph in the order of `subgraphs`: the
    /// subgraph objects standing for it.
    standing: RefCell<HashMap<Name, Rc<Vec<Vec<Name>>>>>,
    /// The providers of each field at each supergraph parent, computed once.
    providers: RefCell<HashMap<(Name, Name), Rc<Providers>>>,
}

/// The subgraphs resolving one field outright at one supergraph parent.
struct Providers {
    /// Each provider, with what its declaration permits.
    each: Vec<(usize, Permitted)>,
    /// The progressive overrides among them, each the overriding subgraph and the one it
    /// overrides.
    pairs: Vec<(usize, usize)>,
    /// What every provider permits together.
    all: Permitted,
}

impl<'a> Sharing<'a> {
    pub(super) fn new(
        subgraphs_by_name: &'a IndexMap<Arc<str>, ValidFederationSchema>,
    ) -> Result<Self, ComparisonError> {
        let mut names = Vec::new();
        let mut subgraphs = Vec::new();
        for (name, schema) in subgraphs_by_name {
            names.push(name.clone());
            subgraphs.push(Subgraph::new(schema)?);
        }
        Ok(Sharing {
            names,
            subgraphs,
            standing: Default::default(),
            providers: Default::default(),
        })
    }

    /// Per subgraph, the objects standing for the supergraph object `parent` through the
    /// declaration correspondence: the object of its name, and the `@interfaceObject` of each
    /// interface it implements, each kept where the subgraph declares it so. Examined the first
    /// time `parent` is asked about.
    ///
    /// The work for a parent asked is its candidates: in each subgraph, its own name and each
    /// interface it implements, each decided by [`Subgraph::stands_for`] in a constant number of
    /// map lookups. No member of an interface is visited, however many objects implement it.
    fn standing_for(
        &self,
        algebra: &Algebra,
        supergraph: &SchemaView<'_>,
        parent: &Name,
    ) -> Rc<Vec<Vec<Name>>> {
        if let Some(standing) = self.standing.borrow().get(parent) {
            return standing.clone();
        }
        let implemented = match supergraph.schema().schema().types.get(parent) {
            Some(ExtendedType::Object(object)) => Some(&object.implements_interfaces),
            _ => None,
        };
        let standing: Vec<Vec<Name>> = self
            .subgraphs
            .iter()
            .map(|subgraph| {
                std::iter::once(parent)
                    .chain(
                        implemented
                            .into_iter()
                            .flatten()
                            .map(|interface| &**interface),
                    )
                    .filter(|candidate| {
                        algebra.work.sharing_candidates.add(1);
                        subgraph.stands_for(&algebra.work, supergraph, candidate, parent)
                    })
                    .inspect(|_| algebra.work.sharing_entries.add(1))
                    .cloned()
                    .collect()
            })
            .collect();
        let standing = Rc::new(standing);
        self.standing
            .borrow_mut()
            .insert(parent.clone(), standing.clone());
        standing
    }

    /// What the providers of `field` at the supergraph object `parent` permit its result to be,
    /// for a contribution `asked` of the subgraph of that name.
    ///
    /// The asked subgraph is among them when it resolves the field outright, and its own
    /// declaration permits everything its result holds, so what every provider permits restricts
    /// its result as what every other one does. A progressive override's two declarations are not
    /// providers for each other, and each constrains every other subgraph.
    pub(super) fn permitted(
        &self,
        algebra: &Algebra,
        supergraph: &SchemaView<'_>,
        asked: &str,
        parent: &Name,
        field: &Name,
    ) -> Result<Permitted, ComparisonError> {
        let providers = self.providers(algebra, supergraph, parent, field)?;
        let asked = self.names.iter().position(|name| **name == *asked);
        let partner = |index: usize| {
            providers.pairs.iter().any(|&(overriding, overridden)| {
                (asked == Some(overriding) && index == overridden)
                    || (asked == Some(overridden) && index == overriding)
            })
        };
        if !providers.each.iter().any(|&(index, _)| partner(index)) {
            return Ok(providers.all);
        }
        Ok(providers
            .each
            .iter()
            .filter(|&&(index, _)| !partner(index))
            .fold(Permitted::Anything, |all, &(_, own)| {
                both(algebra, all, own)
            }))
    }

    /// What each subgraph resolving `field` outright at the supergraph object `parent` permits.
    fn providers(
        &self,
        algebra: &Algebra,
        supergraph: &SchemaView<'_>,
        parent: &Name,
        field: &Name,
    ) -> Result<Rc<Providers>, ComparisonError> {
        let key = (parent.clone(), field.clone());
        if let Some(providers) = self.providers.borrow().get(&key) {
            return Ok(providers.clone());
        }
        let standing = self.standing_for(algebra, supergraph, parent);
        let mut each = Vec::new();
        let mut pairs = Vec::new();
        for (index, provider) in self.subgraphs.iter().enumerate() {
            let mut own = Permitted::Anything;
            for provider_parent in &standing[index] {
                algebra.work.sharing_parents.add(1);
                if provider.field(provider_parent, field).is_none()
                    || !provider.resolves(provider_parent, field)
                {
                    continue;
                }
                if let Some(from) = provider.progressively_overrides(provider_parent, field)?
                    && let Some(overridden) = self.names.iter().position(|name| **name == *from)
                {
                    pairs.push((index, overridden));
                }
                own = match provider.field_runtime_types(
                    &algebra.work,
                    supergraph,
                    provider_parent,
                    field,
                ) {
                    Some(cases) => both(
                        algebra,
                        own,
                        Permitted::Only(algebra.objects(cases.iter().map(|(_, object)| object))),
                    ),
                    None => Permitted::Unjustified,
                };
            }
            if own != Permitted::Anything {
                each.push((index, own));
            }
        }
        let all = each.iter().fold(Permitted::Anything, |all, &(_, own)| {
            both(algebra, all, own)
        });
        let providers = Rc::new(Providers { each, pairs, all });
        self.providers.borrow_mut().insert(key, providers.clone());
        Ok(providers)
    }
}

/// What two restrictions permit together.
fn both(algebra: &Algebra, left: Permitted, right: Permitted) -> Permitted {
    match (left, right) {
        (Permitted::Unjustified, _) | (_, Permitted::Unjustified) => Permitted::Unjustified,
        (Permitted::Anything, other) | (other, Permitted::Anything) => other,
        (Permitted::Only(left), Permitted::Only(right)) => {
            Permitted::Only(algebra.intersect(left, right))
        }
    }
}
