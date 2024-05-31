//! # GraphQL subgraph query optimization.
//!
//! This module contains the logic to optimize (or "compress") a subgraph query by using fragments
//! (either reusing existing ones in the original query or generating new ones).
//!
//! ## Selection/SelectionSet intersection/minus operations
//! These set-theoretic operation methods are used to compute the optimized selection set.
//!
//! ## Collect applicable fragments at given type.
//! This is only the first filtering step. Further validation is needed to check if they can merge
//! with other fields and fragment selections.
//!
//! ## Field validation
//! `FieldsConflictMultiBranchValidator` (and `FieldsConflictValidator`) are used to check if
//! modified subgraph GraphQL queries are still valid, since adding fragments can introduce
//! conflicts.
//!
//! ## Matching fragments with selection set
//! `try_optimize_with_fragments` tries to match all applicable fragments one by one.
//! They are expanded into selection sets in order to match against given selection set.
//! Set-intersection/-minus/-containment operations are used to narrow down to fewer number of
//! fragments that can be used to optimize the selection set. If there is a single fragment that
//! covers the full selection set, then that fragment is used. Otherwise, we attempted to reduce
//! the number of fragments applied (but optimality is not guaranteed yet).
//!
//! ## Retain certain fragments in selection sets while expanding the rest
//! Unlike the `expand_all_fragments` method, this methods retains the listed fragments.
//!
//! ## Optimize (or reduce) the named fragments in the query
//! Optimization of named fragment definitions in query documents based on the usage of
//! fragments in (optimized) operations.
//!
//! ## `optimize` methods (putting everything together)
//! Recursive optimization of selection and selection sets.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Not;
use std::sync::Arc;

use apollo_compiler::executable;
use apollo_compiler::executable::Name;
use apollo_compiler::Node;

use super::operation::CollectedFieldInSet;
use super::operation::Containment;
use super::operation::ContainmentOptions;
use super::operation::Field;
use super::operation::FieldSelection;
use super::operation::Fragment;
use super::operation::FragmentSpreadSelection;
use super::operation::InlineFragmentSelection;
use super::operation::NamedFragments;
use super::operation::NormalizeSelectionOption;
use super::operation::Operation;
use super::operation::Selection;
use super::operation::SelectionKey;
use super::operation::SelectionMapperReturn;
use super::operation::SelectionOrSet;
use super::operation::SelectionSet;
use crate::error::FederationError;
use crate::schema::position::CompositeTypeDefinitionPosition;

//=============================================================================
// Selection/SelectionSet intersection/minus operations

impl Selection {
    // PORT_NOTE: The definition of `minus` and `intersection` functions when either `self` or
    // `other` has no sub-selection seems unintuitive. Why `apple.minus(orange) = None` and
    // `apple.intersection(orange) = apple`?

    /// Computes the set-subtraction (self - other) and returns the result (the difference between
    /// self and other).
    /// If there are respective sub-selections, then we compute their diffs and add them (if not
    /// empty). Otherwise, we have no diff.
    fn minus(&self, other: &Selection) -> Result<Option<Selection>, FederationError> {
        if let (Some(self_sub_selection), Some(other_sub_selection)) =
            (self.selection_set()?, other.selection_set()?)
        {
            let diff = self_sub_selection.minus(other_sub_selection)?;
            if !diff.is_empty() {
                return self
                    .with_updated_selections(
                        self_sub_selection.type_position.clone(),
                        diff.into_iter().map(|(_, v)| v),
                    )
                    .map(Some);
            }
        }
        Ok(None)
    }

    /// Computes the set-intersection of self and other
    /// - If there are respective sub-selections, then we compute their intersections and add them
    ///   (if not empty).
    /// - Otherwise, the intersection is same as `self`.
    fn intersection(&self, other: &Selection) -> Result<Option<Selection>, FederationError> {
        if let (Some(self_sub_selection), Some(other_sub_selection)) =
            (self.selection_set()?, other.selection_set()?)
        {
            let common = self_sub_selection.intersection(other_sub_selection)?;
            if !common.is_empty() {
                return self
                    .with_updated_selections(
                        self_sub_selection.type_position.clone(),
                        common.into_iter().map(|(_, v)| v),
                    )
                    .map(Some);
            }
        }
        Ok(Some(self.clone()))
    }
}

impl SelectionSet {
    /// Performs set-subtraction (self - other) and returns the result (the difference between self
    /// and other).
    pub(crate) fn minus(&self, other: &SelectionSet) -> Result<SelectionSet, FederationError> {
        let iter = self
            .selections
            .iter()
            .map(|(k, v)| {
                if let Some(other_v) = other.selections.get(k) {
                    v.minus(other_v)
                } else {
                    Ok(Some(v.clone()))
                }
            })
            .collect::<Result<Vec<_>, _>>()? // early break in case of Err
            .into_iter()
            .flatten();
        Ok(SelectionSet::from_raw_selections(
            self.schema.clone(),
            self.type_position.clone(),
            iter,
        ))
    }

    /// Computes the set-intersection of self and other
    fn intersection(&self, other: &SelectionSet) -> Result<SelectionSet, FederationError> {
        if self.is_empty() {
            return Ok(self.clone());
        }
        if other.is_empty() {
            return Ok(other.clone());
        }

        let iter = self
            .selections
            .iter()
            .map(|(k, v)| {
                if let Some(other_v) = other.selections.get(k) {
                    v.intersection(other_v)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<Vec<_>, _>>()? // early break in case of Err
            .into_iter()
            .flatten();
        Ok(SelectionSet::from_raw_selections(
            self.schema.clone(),
            self.type_position.clone(),
            iter,
        ))
    }
}

//=============================================================================
// Collect applicable fragments at given type.

impl Fragment {
    /// Whether this fragment may apply _directly_ at the provided type, meaning that the fragment
    /// sub-selection (_without_ the fragment condition, hence the "directly") can be normalized at
    /// `ty` without overly "widening" the runtime types.
    ///
    /// * `ty` - the type at which we're looking at applying the fragment
    //
    // The runtime types of the fragment condition must be at least as general as those of the
    // provided `ty`. Otherwise, putting it at `ty` without its condition would "generalize"
    // more than the fragment meant to (and so we'd "widen" the runtime types more than what the
    // query meant to.
    fn can_apply_directly_at_type(
        &self,
        ty: &CompositeTypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        // Short-circuit #1: the same type => trivially true.
        if self.type_condition_position == *ty {
            return Ok(true);
        }

        // Short-circuit #2: The type condition is not an abstract type (too restrictive).
        // - It will never cover all of the runtime types of `ty` unless it's the same type, which is
        //   already checked by short-circuit #1.
        if !self.type_condition_position.is_abstract_type() {
            return Ok(false);
        }

        // Short-circuit #3: The type condition is not an object (due to short-circuit #2) nor a
        // union type, but the `ty` may be too general.
        // - In other words, the type condition must be an interface but `ty` is a (different)
        //   interface or a union.
        // PORT_NOTE: In JS, this check was later on the return statement (negated). But, this
        //            should be checked before `possible_runtime_types` check, since this is
        //            cheaper to execute.
        // PORT_NOTE: This condition may be too restrictive (potentially a bug leading to
        //            suboptimal compression). If ty is a union whose members all implements the
        //            type condition (interface). Then, this function should've returned true.
        //            Thus, `!ty.is_union_type()` might be needed.
        if !self.type_condition_position.is_union_type() && !ty.is_object_type() {
            return Ok(false);
        }

        // Check if the type condition is a superset of the provided type.
        // - The fragment condition must be at least as general as the provided type.
        let condition_types = self
            .schema
            .possible_runtime_types(self.type_condition_position.clone())?;
        let ty_types = self.schema.possible_runtime_types(ty.clone())?;
        Ok(condition_types.is_superset(&ty_types))
    }
}

impl NamedFragments {
    /// Returns a list of fragments that can be applied directly at the given type.
    fn get_all_may_apply_directly_at_type(
        &self,
        ty: &CompositeTypeDefinitionPosition,
    ) -> Result<Vec<Node<Fragment>>, FederationError> {
        self.iter()
            .filter_map(|fragment| {
                fragment
                    .can_apply_directly_at_type(ty)
                    .map(|can_apply| can_apply.then_some(fragment.clone()))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()
    }
}

//=============================================================================
// Field validation

// PORT_NOTE: Not having a validator and having a FieldsConflictValidator with empty
// `by_response_name` map has no difference in behavior. So, we could drop the `Option` from
// `Option<FieldsConflictValidator>`. However, `None` validator makes it clearer that validation is
// unnecessary.
struct FieldsConflictValidator {
    by_response_name: HashMap<Name, HashMap<Field, Option<Arc<FieldsConflictValidator>>>>,
}

impl FieldsConflictValidator {
    fn from_selection_set(selection_set: &SelectionSet) -> Self {
        Self::for_level(&selection_set.fields_in_set())
    }

    fn for_level(level: &[CollectedFieldInSet]) -> Self {
        // Group `level`'s fields by the response-name/field
        let mut at_level: HashMap<Name, HashMap<Field, Option<Vec<CollectedFieldInSet>>>> =
            HashMap::new();
        for collected_field in level {
            let response_name = collected_field.field().field.data().response_name();
            let at_response_name = at_level.entry(response_name).or_default();
            if let Some(ref field_selection_set) = collected_field.field().selection_set {
                at_response_name
                    .entry(collected_field.field().field.clone())
                    .or_default()
                    .get_or_insert_with(Default::default)
                    .extend(field_selection_set.fields_in_set());
            } else {
                // Note that whether a `FieldSelection` has a sub-selection set or not is entirely
                // determined by whether the field type is a composite type or not, so even if
                // we've seen a previous version of `field` before, we know it's guaranteed to have
                // no selection set here, either. So the `set` below may overwrite a previous
                // entry, but it would be a `None` so no harm done.
                at_response_name.insert(collected_field.field().field.clone(), None);
            }
        }

        // Collect validators per response-name/field
        let mut by_response_name = HashMap::new();
        for (response_name, fields) in at_level {
            let mut at_response_name: HashMap<Field, Option<Arc<FieldsConflictValidator>>> =
                HashMap::new();
            for (field, collected_fields) in fields {
                let validator = collected_fields
                    .map(|collected_fields| Arc::new(Self::for_level(&collected_fields)));
                at_response_name.insert(field, validator);
            }
            by_response_name.insert(response_name, at_response_name);
        }
        Self { by_response_name }
    }

    fn for_field(&self, field: &Field) -> Vec<Arc<Self>> {
        let Some(by_response_name) = self.by_response_name.get(&field.data().response_name())
        else {
            return Vec::new();
        };
        by_response_name.values().flatten().cloned().collect()
    }

    fn has_same_response_shape(
        &self,
        other: &FieldsConflictValidator,
    ) -> Result<bool, FederationError> {
        for (response_name, self_fields) in self.by_response_name.iter() {
            let Some(other_fields) = other.by_response_name.get(response_name) else {
                continue;
            };

            for (self_field, self_validator) in self_fields {
                for (other_field, other_validator) in other_fields {
                    if !self_field.types_can_be_merged(other_field)? {
                        return Ok(false);
                    }

                    if let Some(self_validator) = self_validator {
                        if let Some(other_validator) = other_validator {
                            if !self_validator.has_same_response_shape(other_validator)? {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
        }
        Ok(true)
    }

    fn do_merge_with(&self, other: &FieldsConflictValidator) -> Result<bool, FederationError> {
        for (response_name, self_fields) in self.by_response_name.iter() {
            let Some(other_fields) = other.by_response_name.get(response_name) else {
                continue;
            };

            // We're basically checking
            // [FieldsInSetCanMerge](https://spec.graphql.org/draft/#FieldsInSetCanMerge()), but
            // from 2 set of fields (`self_fields` and `other_fields`) of the same response that we
            // know individually merge already.
            for (self_field, self_validator) in self_fields {
                for (other_field, other_validator) in other_fields {
                    if !self_field.types_can_be_merged(other_field)? {
                        return Ok(false);
                    }

                    let p1 = self_field.parent_type_position();
                    let p2 = other_field.parent_type_position();
                    if p1 == p2 || !p1.is_object_type() || !p2.is_object_type() {
                        // Additional checks of `FieldsInSetCanMerge` when same parent type or one
                        // isn't object
                        if self_field.data().name() != other_field.data().name()
                            || self_field.data().arguments != other_field.data().arguments
                        {
                            return Ok(false);
                        }
                        if let Some(self_validator) = self_validator {
                            if let Some(other_validator) = other_validator {
                                if !self_validator.do_merge_with(other_validator)? {
                                    return Ok(false);
                                }
                            }
                        }
                    } else {
                        // Otherwise, the sub-selection must pass
                        // [SameResponseShape](https://spec.graphql.org/draft/#SameResponseShape()).
                        if let Some(self_validator) = self_validator {
                            if let Some(other_validator) = other_validator {
                                if !self_validator.has_same_response_shape(other_validator)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(true)
    }

    fn do_merge_with_all<'a>(
        &self,
        mut iter: impl Iterator<Item = &'a FieldsConflictValidator>,
    ) -> Result<bool, FederationError> {
        iter.try_fold(true, |acc, v| Ok(acc && self.do_merge_with(v)?))
    }
}

struct FieldsConflictMultiBranchValidator {
    validators: Vec<Arc<FieldsConflictValidator>>,
    used_spread_trimmed_part_at_level: Vec<Arc<FieldsConflictValidator>>,
}

impl FieldsConflictMultiBranchValidator {
    fn new(validators: Vec<Arc<FieldsConflictValidator>>) -> Self {
        Self {
            validators,
            used_spread_trimmed_part_at_level: Vec::new(),
        }
    }

    fn from_initial_validator(validator: FieldsConflictValidator) -> Self {
        Self {
            validators: vec![Arc::new(validator)],
            used_spread_trimmed_part_at_level: Vec::new(),
        }
    }

    fn for_field(&self, field: &Field) -> Self {
        let for_all_branches = self.validators.iter().flat_map(|v| v.for_field(field));
        Self::new(for_all_branches.collect())
    }

    // When this method is used in the context of `try_optimize_with_fragments`, we know that the
    // fragment, restricted to the current parent type, matches a subset of the sub-selection.
    // However, there is still one case we we cannot use it that we need to check, and this is if
    // using the fragment would create a field "conflict" (in the sense of the graphQL spec
    // [`FieldsInSetCanMerge`](https://spec.graphql.org/draft/#FieldsInSetCanMerge())) and thus
    // create an invalid selection. To be clear, `at_type.selections` cannot create a conflict,
    // since it is a subset of the target selection set and it is valid by itself. *But* there may
    // be some part of the fragment that is not `at_type.selections` due to being "dead branches"
    // for type `parent_type`. And while those branches _are_ "dead" as far as execution goes, the
    // `FieldsInSetCanMerge` validation does not take this into account (it's 1st step says
    // "including visiting fragments and inline fragments" but has no logic regarding ignoring any
    // fragment that may not apply due to the intersection of runtime types between multiple
    // fragment being empty).
    fn check_can_reuse_fragment_and_track_it(
        &mut self,
        fragment_restriction: &FragmentRestrictionAtType,
    ) -> Result<bool, FederationError> {
        // No validator means that everything in the fragment selection was part of the selection
        // we're optimizing away (by using the fragment), and we know the original selection was
        // ok, so nothing to check.
        let Some(validator) = &fragment_restriction.validator else {
            return Ok(true); // Nothing to check; Trivially ok.
        };

        if !validator.do_merge_with_all(self.validators.iter().map(Arc::as_ref))? {
            return Ok(false);
        }

        // We need to make sure the trimmed parts of `fragment` merges with the rest of the
        // selection, but also that it merge with any of the trimmed parts of any fragment we have
        // added already.
        // Note: this last condition means that if 2 fragment conflict on their "trimmed" parts,
        // then the choice of which is used can be based on the fragment ordering and selection
        // order, which may not be optimal. This feels niche enough that we keep it simple for now,
        // but we can revisit this decision if we run into real cases that justify it (but making
        // it optimal would be a involved in general, as in theory you could have complex
        // dependencies of fragments that conflict, even cycles, and you need to take the size of
        // fragments into account to know what's best; and even then, this could even depend on
        // overall usage, as it can be better to reuse a fragment that is used in other places,
        // than to use one for which it's the only usage. Adding to all that the fact that conflict
        // can happen in sibling branches).
        if !validator.do_merge_with_all(
            self.used_spread_trimmed_part_at_level
                .iter()
                .map(Arc::as_ref),
        )? {
            return Ok(false);
        }

        // We're good, but track the fragment.
        self.used_spread_trimmed_part_at_level
            .push(validator.clone());
        Ok(true)
    }
}

//=============================================================================
// Matching fragments with selection set (`try_optimize_with_fragments`)

/// Return type for `expanded_selection_set_at_type` method.
struct FragmentRestrictionAtType {
    /// Selections that are expanded from a given fragment at a given type and then normalized.
    /// - This represents the part of given type's sub-selections that are covered by the fragment.
    selections: SelectionSet,

    /// A runtime validator to check the fragment selections against other fields.
    /// - `None` means that there is nothing to check.
    /// - See `check_can_reuse_fragment_and_track_it` for more details.
    validator: Option<Arc<FieldsConflictValidator>>,
}

impl FragmentRestrictionAtType {
    fn new(selections: SelectionSet, validator: Option<FieldsConflictValidator>) -> Self {
        Self {
            selections,
            validator: validator.map(Arc::new),
        }
    }

    // It's possible that while the fragment technically applies at `parent_type`, it's "rebasing" on
    // `parent_type` is empty, or contains only `__typename`. For instance, suppose we have
    // a union `U = A | B | C`, and then a fragment:
    // ```graphql
    //   fragment F on U {
    //     ... on A {
    //       x
    //     }
    //     ... on B {
    //       y
    //     }
    //   }
    // ```
    // It is then possible to apply `F` when the parent type is `C`, but this ends up selecting
    // nothing at all.
    //
    // Using `F` in those cases is, while not 100% incorrect, at least not productive, and so we
    // skip it that case. This is essentially an optimization.
    fn is_useless(&self) -> bool {
        match self.selections.selections.as_slice().split_first() {
            None => true,

            Some((first, rest)) => rest.is_empty() && first.0.is_typename_field(),
        }
    }
}

impl Fragment {
    /// Computes the expanded selection set of this fragment along with its validator to check
    /// against other fragments applied under the same selection set.
    // PORT_NOTE: The JS version memoizes the result of this function. But, the current Rust port
    // does not.
    fn expanded_selection_set_at_type(
        &self,
        ty: &CompositeTypeDefinitionPosition,
    ) -> Result<FragmentRestrictionAtType, FederationError> {
        let expanded_selection_set = self.selection_set.expand_all_fragments()?;
        let normalized_selection_set = expanded_selection_set.normalize(
            ty,
            /*named_fragments*/ &Default::default(),
            &self.schema,
            NormalizeSelectionOption::NormalizeRecursively,
        )?;

        if !self.type_condition_position.is_object_type() {
            // When the type condition of the fragment is not an object type, the
            // `FieldsInSetCanMerge` rule is more restrictive and any fields can create conflicts.
            // Thus, we have to use the full validator in this case. (see
            // https://github.com/graphql/graphql-spec/issues/1085 for details.)
            return Ok(FragmentRestrictionAtType::new(
                normalized_selection_set.clone(),
                Some(FieldsConflictValidator::from_selection_set(
                    &expanded_selection_set,
                )),
            ));
        }

        // Use a smaller validator for efficiency.
        // Note that `trimmed` is the difference of 2 selections that may not have been normalized
        // on the same parent type, so in practice, it is possible that `trimmed` contains some of
        // the selections that `selectionSet` contains, but that they have been simplified in
        // `selectionSet` in such a way that the `minus` call does not see it. However, it is not
        // trivial to deal with this, and it is fine given that we use trimmed to create the
        // validator because we know the non-trimmed parts cannot create field conflict issues so
        // we're trying to build a smaller validator, but it's ok if trimmed is not as small as it
        // theoretically can be.
        let trimmed = expanded_selection_set.minus(&normalized_selection_set)?;
        let validator = trimmed
            .is_empty()
            .not()
            .then(|| FieldsConflictValidator::from_selection_set(&trimmed));
        Ok(FragmentRestrictionAtType::new(
            normalized_selection_set.clone(),
            validator,
        ))
    }

    /// Checks whether `self` fragment includes the other fragment (`other_fragment_name`).
    //
    // Note that this is slightly different from `self` "using" `other_fragment` in that this
    // essentially checks if the full selection set of `other_fragment` is contained by `self`, so
    // this only look at "top-level" usages.
    //
    // Note that this is guaranteed to return `false` if passed self's name.
    // Note: This is a heuristic looking for the other named fragment used directly in the
    //       selection set. It may not return `true` even though the other fragment's selections
    //       are actually covered by self's selection set.
    // PORT_NOTE: The JS version memoizes the result of this function. But, the current Rust port
    // does not.
    fn includes(&self, other_fragment_name: &Name) -> bool {
        if self.name == *other_fragment_name {
            return false;
        }

        self.selection_set.selections.iter().any(|(selection_key, _)| {
            matches!(
                selection_key,
                SelectionKey::FragmentSpread {fragment_name, directives: _} if fragment_name == other_fragment_name,
            )
        })
    }
}

enum FullMatchingFragmentCondition<'a> {
    ForFieldSelection,
    ForInlineFragmentSelection {
        // the type condition and directives on an inline fragment selection.
        type_condition_position: &'a CompositeTypeDefinitionPosition,
        directives: &'a Arc<executable::DirectiveList>,
    },
}

impl<'a> FullMatchingFragmentCondition<'a> {
    /// Determines whether the given fragment is allowed to match the whole selection set by itself
    /// (without another selection set wrapping it).
    fn check(&self, fragment: &Node<Fragment>) -> bool {
        match self {
            // We can never apply a fragments that has directives on it at the field level.
            Self::ForFieldSelection => fragment.directives.is_empty(),

            // To be able to use a matching inline fragment, it needs to have either no directives,
            // or if it has some, then:
            //  1. All it's directives should also be on the current element.
            //  2. The type condition of this element should be the fragment's condition. because
            // If those 2 conditions are true, we can replace the whole current inline fragment
            // with the match spread and directives will still match.
            Self::ForInlineFragmentSelection {
                type_condition_position,
                directives,
            } => {
                if fragment.directives.is_empty() {
                    return true;
                }

                // PORT_NOTE: The JS version handles `@defer` directive differently. However, Rust
                // version can't have `@defer` at this point (see comments on `enum SelectionKey`
                // definition)
                fragment.type_condition_position == **type_condition_position
                    && fragment
                        .directives
                        .iter()
                        .all(|d1| directives.iter().any(|d2| d1 == d2))
            }
        }
    }
}

/// The return type for `SelectionSet::try_optimize_with_fragments`.
#[derive(derive_more::From)]
enum SelectionSetOrFragment {
    SelectionSet(SelectionSet),
    Fragment(Node<Fragment>),
}

impl SelectionSet {
    /// Reduce the list of applicable fragments by eliminating ones that are subsumed by another.
    //
    // We have found the list of fragments that applies to some subset of sub-selection. In
    // general, we want to now produce the selection set with spread for those fragments plus
    // any selection that is not covered by any of the fragments. For instance, suppose that
    // `subselection` is `{ a b c d e }` and we have found that `fragment F1 on X { a b c }`
    // and `fragment F2 on X { c d }` applies, then we will generate `{ ...F1 ...F2 e }`.
    //
    // In that example, `c` is covered by both fragments. And this is fine in this example as
    // it is worth using both fragments in general. A special case of this however is if a
    // fragment is entirely included into another. That is, consider that we now have `fragment
    // F1 on X { a ...F2 }` and `fragment F2 on X { b c }`. In that case, the code above would
    // still match both `F1 and `F2`, but as `F1` includes `F2` already, we really want to only
    // use `F1`. So in practice, we filter away any fragment spread that is known to be
    // included in another one that applies.
    //
    // TODO: note that the logic used for this is theoretically a bit sub-optimal. That is, we
    // only check if one of the fragment happens to directly include a spread for another
    // fragment at top-level as in the example above. We do this because it is cheap to check
    // and is likely the most common case of this kind of inclusion. But in theory, we would
    // have `fragment F1 on X { a b c }` and `fragment F2 on X { b c }`, in which case `F2` is
    // still included in `F1`, but we'd have to work harder to figure this out and it's unclear
    // it's a good tradeoff. And while you could argue that it's on the user to define its
    // fragments a bit more optimally, it's actually a tad more complex because we're looking
    // at fragments in a particular context/parent type. Consider an interface `I` and:
    // ```graphql
    //   fragment F3 on I {
    //     ... on X {
    //       a
    //     }
    //     ... on Y {
    //       b
    //       c
    //     }
    //   }
    //
    //   fragment F4 on I {
    //     ... on Y {
    //       c
    //     }
    //     ... on Z {
    //       d
    //     }
    //   }
    // ```
    // In that case, neither fragment include the other per-se. But what if we have
    // sub-selection `{ b c }` but where parent type is `Y`. In that case, both `F3` and `F4`
    // applies, and in that particular context, `F3` is fully included in `F4`. Long story
    // short, we'll currently return `{ ...F3 ...F4 }` in that case, but it would be
    // technically better to return only `F4`. However, this feels niche, and it might be
    // costly to verify such inclusions, so not doing it for now.
    fn reduce_applicable_fragments(
        applicable_fragments: &mut Vec<(Node<Fragment>, FragmentRestrictionAtType)>,
    ) {
        // Note: It's not possible for two fragments to include each other. So, we don't need to
        //       worry about inclusion cycles.
        let included_fragments: HashSet<Name> = applicable_fragments
            .iter()
            .filter(|(fragment, _)| {
                applicable_fragments
                    .iter()
                    .any(|(other_fragment, _)| other_fragment.includes(&fragment.name))
            })
            .map(|(fragment, _)| fragment.name.clone())
            .collect();

        applicable_fragments.retain(|(fragment, _)| !included_fragments.contains(&fragment.name));
    }

    /// Try to optimize the selection set by re-using existing fragments.
    /// Returns either
    /// - a new selection set partially optimized by re-using given `fragments`, or
    /// - a single fragment that covers the full selection set.
    // PORT_NOTE: Moved from `Selection` class in JS code to SelectionSet struct in Rust.
    // PORT_NOTE: `parent_type` argument seems always to be the same as `self.type_position`.
    fn try_optimize_with_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        fragments: &NamedFragments,
        validator: &mut FieldsConflictMultiBranchValidator,
        full_match_condition: FullMatchingFragmentCondition,
    ) -> Result<SelectionSetOrFragment, FederationError> {
        // We limit to fragments whose selection could be applied "directly" at `parent_type`,
        // meaning without taking the fragment condition into account. The idea being that if the
        // fragment condition would be needed inside `parent_type`, then that condition will not
        // have been "normalized away" and so we want for this very call to be called on the
        // fragment whose type _is_ the fragment condition (at which point, this
        // `can_apply_directly_at_type` method will apply. Also note that this is because we have
        // this restriction that calling `expanded_selection_set_at_type` is ok.
        let candidates = fragments.get_all_may_apply_directly_at_type(parent_type)?;
        if candidates.is_empty() {
            return Ok(self.clone().into()); // Not optimizable
        }

        // First, we check which of the candidates do apply inside the selection set, if any. If we
        // find a candidate that applies to the whole selection set, then we stop and only return
        // that one candidate. Otherwise, we cumulate in `applicable_fragments` the list of fragments
        // that applies to a subset.
        let mut applicable_fragments = Vec::new();
        for candidate in candidates {
            let at_type = candidate.expanded_selection_set_at_type(parent_type)?;
            if at_type.is_useless() {
                continue;
            }
            if !validator.check_can_reuse_fragment_and_track_it(&at_type)? {
                // We cannot use it at all, so no point in adding to `applicable_fragments`.
                continue;
            }

            // As we check inclusion, we ignore the case where the fragment queries __typename
            // but the `self` does not. The rational is that querying `__typename`
            // unnecessarily is mostly harmless (it always works and it's super cheap) so we
            // don't want to not use a fragment just to save querying a `__typename` in a few
            // cases. But the underlying context of why this matters is that the query planner
            // always requests __typename for abstract type, and will do so in fragments too,
            // but we can have a field that _does_ return an abstract type within a fragment,
            // but that _does not_ end up returning an abstract type when applied in a "more
            // specific" context (think a fragment on an interface I1 where a inside field
            // returns another interface I2, but applied in the context of a implementation
            // type of I1 where that particular field returns an implementation of I2 rather
            // than I2 directly; we would have added __typename to the fragment (because it's
            // all interfaces), but the selection itself, which only deals with object type,
            // may not have __typename requested; using the fragment might still be a good
            // idea, and querying __typename needlessly is a very small price to pay for that).
            let res = self.containment(
                &at_type.selections,
                ContainmentOptions {
                    ignore_missing_typename: true,
                },
            );
            if matches!(res, Containment::NotContained) {
                continue; // Not eligible; Skip it.
            }
            if matches!(res, Containment::Equal) && full_match_condition.check(&candidate) {
                // Special case: Found a fragment that covers the full selection set.
                return Ok(candidate.into());
            }
            // Note that if a fragment applies to only a subset of the sub-selections, then we
            // really only can use it if that fragment is defined _without_ directives.
            if !candidate.directives.is_empty() {
                continue; // Not eligible as a partial selection; Skip it.
            }
            applicable_fragments.push((candidate, at_type));
        }

        if applicable_fragments.is_empty() {
            return Ok(self.clone().into()); // Not optimizable
        }

        // Narrow down the list of applicable fragments by removing those that are included in
        // another.
        Self::reduce_applicable_fragments(&mut applicable_fragments);

        // Build a new optimized selection set.
        let mut not_covered_so_far = self.clone();
        let mut optimized = SelectionSet::empty(self.schema.clone(), self.type_position.clone());
        for (fragment, at_type) in applicable_fragments {
            let not_covered = self.minus(&at_type.selections)?;
            not_covered_so_far = not_covered_so_far.intersection(&not_covered)?;

            // PORT_NOTE: The JS version uses `parent_type` as the "sourceType", which may be
            //            different from `fragment.type_condition_position`. But, Rust version does
            //            not have "sourceType" field for `FragmentSpreadSelection`.
            let fragment_selection = FragmentSpreadSelection::from_fragment(
                &fragment,
                /*directives*/ &Default::default(),
            );
            Arc::make_mut(&mut optimized.selections).insert(fragment_selection.into());
        }

        Arc::make_mut(&mut optimized.selections).extend_ref(&not_covered_so_far.selections);
        Ok(SelectionSet::make_selection_set(
            &self.schema,
            parent_type,
            optimized.selections.values().map(std::iter::once),
            fragments,
        )?
        .into())
    }
}

//=============================================================================
// Retain fragments in selection sets while expanding the rest

impl Selection {
    /// Expand fragments that are not in the `fragments_to_keep`.
    // PORT_NOTE: The JS version's name was `expandFragments`, which was confusing with
    //            `expand_all_fragments`. So, it was renamed to `retain_fragments`.
    fn retain_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        fragments_to_keep: &NamedFragments,
    ) -> Result<SelectionOrSet, FederationError> {
        match self {
            Selection::FragmentSpread(fragment) => {
                if fragments_to_keep.contains(&fragment.spread.data().fragment_name) {
                    // Keep this spread
                    Ok(self.clone().into())
                } else {
                    // Expand the fragment
                    let expanded_sub_selections =
                        fragment.selection_set.retain_fragments(fragments_to_keep)?;
                    if *parent_type == fragment.spread.data().type_condition_position {
                        // The fragment is of the same type as the parent, so we can just use
                        // the expanded sub-selections directly.
                        Ok(expanded_sub_selections.into())
                    } else {
                        // Create an inline fragment since type condition is necessary.
                        let inline = InlineFragmentSelection::from_fragment_spread_selection(
                            parent_type.clone(),
                            fragment,
                        )?;
                        Ok(Selection::from(inline).into())
                    }
                }
            }

            // Otherwise, expand the sub-selections.
            _ => Ok(self
                .map_selection_set(|selection_set| {
                    Ok(Some(selection_set.retain_fragments(fragments_to_keep)?))
                })?
                .into()),
        }
    }
}

// Note: `retain_fragments` methods may return a selection or a selection set.
impl From<SelectionOrSet> for SelectionMapperReturn {
    fn from(value: SelectionOrSet) -> Self {
        match value {
            SelectionOrSet::Selection(selection) => selection.into(),
            SelectionOrSet::SelectionSet(selections) => {
                // The items in a selection set needs to be cloned here, since it's sub-selections
                // are contained in an `Arc`.
                Vec::from_iter(selections.selections.values().cloned()).into()
            }
        }
    }
}

impl SelectionSet {
    /// Expand fragments that are not in the `fragments_to_keep`.
    // PORT_NOTE: The JS version's name was `expandFragments`, which was confusing with
    //            `expand_all_fragments`. So, it was renamed to `retain_fragments`.
    fn retain_fragments(
        &self,
        fragments_to_keep: &NamedFragments,
    ) -> Result<Self, FederationError> {
        self.lazy_map(fragments_to_keep, |selection| {
            Ok(selection
                .retain_fragments(&self.type_position, fragments_to_keep)?
                .into())
        })
    }
}

//=============================================================================
// Optimize (or reduce) the named fragments in the query
//
// Things to consider:
// - Unused fragment definitions can be dropped without an issue.
// - Dropping low-usage named fragments and expanding them may insert other fragments resulting in
//   increased usage of those inserted.
//
// Example:
//  ```graphql
//   query {
//      ...F1
//   }
//
//   fragment F1 {
//     a { ...F2 }
//     b { ...F2 }
//   }
//
//   fragment F2 {
//      // something
//   }
//  ```
//  then at this point where we've only counted usages in the query selection, `usages` will be
//  `{ F1: 1, F2: 0 }`. But we do not want to expand _both_ F1 and F2. Instead, we want to expand
//  F1 first, and then realize that this increases F2 usages to 2, which means we stop there and keep F2.

impl NamedFragments {
    /// Compute the reduced set of NamedFragments that are used in the selection set at least
    /// `min_usage_to_optimize` times. Also, computes the new selection set that uses only the
    /// reduced set of fragments by expanding the other ones.
    fn reduce(
        &mut self,
        selection_set: &SelectionSet,
        min_usage_to_optimize: u32,
    ) -> Result<SelectionSet, FederationError> {
        // Initial computation of fragment usages in `selection_set`.
        let mut usages = HashMap::new();
        selection_set.collect_used_fragment_names(&mut usages);

        // Short-circuiting: Nothing was used => Drop everything (selection_set is unchanged).
        if usages.is_empty() {
            self.retain(|_, _| false);
            return Ok(selection_set.clone());
        }

        // Determine which one to retain.
        // - Calculate the usage count of each fragment in both query and other fragment definitions.
        //   - If a fragment is to keep, fragments used in it are counted.
        //   - If a fragment is to drop, fragments used in it are counted and multiplied by its usage.
        // - Decide in reverse dependency order, so that at each step, the fragment being visited
        //   has following properties:
        //   - It is either indirectly used by a previous fragment; Or, not used directly by any
        //     one visited & retained before.
        //   - Its usage count should be correctly calculated as if dropped fragments were expanded.
        // - We take advantage of the fact that `NamedFragments` is already sorted in dependency
        //   order.
        let min_usage_to_optimize: i32 = min_usage_to_optimize.try_into().unwrap_or(i32::MAX);
        let original_size = self.size();
        for fragment in self.iter_rev() {
            let usage_count = usages.get(&fragment.name).copied().unwrap_or_default();
            if usage_count >= min_usage_to_optimize {
                // Count indirect usages within the fragment definition.
                fragment.collect_used_fragment_names(&mut usages);
            } else {
                // Compute the new usage count after expanding the `fragment`.
                Self::update_usages(&mut usages, fragment, usage_count);
            }
        }
        self.retain(|name, _fragment| {
            let usage_count = usages.get(name).copied().unwrap_or_default();
            usage_count >= min_usage_to_optimize
        });

        // Short-circuiting: Nothing was dropped (fully used) => Nothing to change.
        if self.size() == original_size {
            return Ok(selection_set.clone());
        }

        // Update the fragment definitions in `self` after reduction.
        // Note: This is an unfortunate clone, since `self` can't be passed to `retain_fragments`,
        //       while being mutated.
        let fragments_to_keep = self.clone();
        for (_, fragment) in self.iter_mut() {
            Node::make_mut(fragment).selection_set = fragment
                .selection_set
                .retain_fragments(&fragments_to_keep)?;
        }

        // Compute the new selection set based on the new reduced set of fragments.
        selection_set.retain_fragments(self)
    }

    fn update_usages(usages: &mut HashMap<Name, i32>, fragment: &Node<Fragment>, usage_count: i32) {
        let mut inner_usages = HashMap::new();
        fragment.collect_used_fragment_names(&mut inner_usages);

        for (name, inner_count) in inner_usages {
            *usages.entry(name).or_insert(0) += inner_count * usage_count;
        }
    }
}

//=============================================================================
// `optimize` methods (putting everything together)

impl Selection {
    fn optimize(
        &self,
        fragments: &NamedFragments,
        validator: &mut FieldsConflictMultiBranchValidator,
    ) -> Result<Selection, FederationError> {
        match self {
            Selection::Field(field) => Ok(field.optimize(fragments, validator)?.into()),
            Selection::FragmentSpread(_) => Ok(self.clone()), // Do nothing
            Selection::InlineFragment(inline_fragment) => {
                Ok(inline_fragment.optimize(fragments, validator)?.into())
            }
        }
    }
}

impl FieldSelection {
    fn optimize(
        &self,
        fragments: &NamedFragments,
        validator: &mut FieldsConflictMultiBranchValidator,
    ) -> Result<Self, FederationError> {
        let Some(base_composite_type): Option<CompositeTypeDefinitionPosition> =
            self.field.data().output_base_type()?.try_into().ok()
        else {
            return Ok(self.clone());
        };
        let Some(ref selection_set) = self.selection_set else {
            return Ok(self.clone());
        };

        let mut field_validator = validator.for_field(&self.field);

        // First, see if we can reuse fragments for the selection of this field.
        let opt = selection_set.try_optimize_with_fragments(
            &base_composite_type,
            fragments,
            &mut field_validator,
            FullMatchingFragmentCondition::ForFieldSelection,
        )?;

        let mut optimized;
        match opt {
            SelectionSetOrFragment::Fragment(fragment) => {
                let fragment_selection = FragmentSpreadSelection::from_fragment(
                    &fragment,
                    /*directives*/ &Default::default(),
                );
                optimized =
                    SelectionSet::from_selection(base_composite_type, fragment_selection.into());
            }
            SelectionSetOrFragment::SelectionSet(selection_set) => {
                optimized = selection_set;
            }
        }
        optimized = optimized.optimize(fragments, &mut field_validator)?;
        Ok(self.with_updated_selection_set(Some(optimized)))
    }
}

/// Return type for `InlineFragmentSelection::optimize`.
#[derive(derive_more::From)]
enum InlineOrFragmentSelection {
    // Note: Enum variants are named to match those of `Selection`.
    InlineFragment(InlineFragmentSelection),
    FragmentSpread(FragmentSpreadSelection),
}

impl From<InlineOrFragmentSelection> for Selection {
    fn from(value: InlineOrFragmentSelection) -> Self {
        match value {
            InlineOrFragmentSelection::InlineFragment(inline_fragment) => inline_fragment.into(),
            InlineOrFragmentSelection::FragmentSpread(fragment_spread) => fragment_spread.into(),
        }
    }
}

impl InlineFragmentSelection {
    fn optimize(
        &self,
        fragments: &NamedFragments,
        validator: &mut FieldsConflictMultiBranchValidator,
    ) -> Result<InlineOrFragmentSelection, FederationError> {
        let mut optimized = self.selection_set.clone();

        let type_condition_position = &self.inline_fragment.data().type_condition_position;
        if let Some(type_condition_position) = type_condition_position {
            let opt = self.selection_set.try_optimize_with_fragments(
                type_condition_position,
                fragments,
                validator,
                FullMatchingFragmentCondition::ForInlineFragmentSelection {
                    type_condition_position,
                    directives: &self.inline_fragment.data().directives,
                },
            )?;

            match opt {
                SelectionSetOrFragment::Fragment(fragment) => {
                    // We're fully matching the sub-selection. If the fragment condition is also
                    // this element condition, then we can replace the whole element by the spread
                    // (not just the sub-selection).
                    if *type_condition_position == fragment.type_condition_position {
                        // Optimized as `...<fragment>`, dropping the original inline spread (`self`).

                        // Note that `FullMatchingFragmentCondition::ForInlineFragmentSelection`
                        // above guarantees that this element directives are a superset of the
                        // fragment directives. But there can be additional directives, and in that
                        // case they should be kept on the spread.
                        // PORT_NOTE: We are assuming directives on fragment definitions are
                        //            carried over to their spread sites as JS version does, which
                        //            is handled differently in Rust version (see `FragmentSpreadData`).
                        let directives: executable::DirectiveList = self
                            .inline_fragment
                            .data()
                            .directives
                            .iter()
                            .filter(|d1| !fragment.directives.iter().any(|d2| *d1 == d2))
                            .cloned()
                            .collect();
                        return Ok(
                            FragmentSpreadSelection::from_fragment(&fragment, &directives).into(),
                        );
                    } else {
                        // Otherwise, we keep this element and use a sub-selection with just the spread.
                        // Optimized as `...on <type_condition_position> { ...<fragment> }`
                        optimized = SelectionSet::from_selection(
                            type_condition_position.clone(),
                            FragmentSpreadSelection::from_fragment(
                                &fragment,
                                /*directives*/ &Default::default(),
                            )
                            .into(),
                        );
                        // fall-through
                    }
                }
                SelectionSetOrFragment::SelectionSet(selection_set) => {
                    optimized = selection_set;
                    // fall-through
                }
            }
        }

        // Then, recurse inside the field sub-selection (note that if we matched some fragments
        // above, this recursion will "ignore" those as `FragmentSpreadSelection`'s `optimize()` is
        // a no-op).
        optimized = optimized.optimize(fragments, validator)?;
        Ok(InlineFragmentSelection {
            inline_fragment: self.inline_fragment.clone(),
            selection_set: optimized,
        }
        .into())
    }
}

impl SelectionSet {
    /// Recursively call `optimize` on each selection in the selection set.
    fn optimize(
        &self,
        fragments: &NamedFragments,
        validator: &mut FieldsConflictMultiBranchValidator,
    ) -> Result<SelectionSet, FederationError> {
        self.lazy_map(fragments, |selection| {
            Ok(vec![selection.optimize(fragments, validator)?].into())
        })
    }

    /// Specialized version of `optimize` for top-level sub-selections under Operation
    /// or Fragment.
    pub(crate) fn optimize_at_root(
        &mut self,
        fragments: &NamedFragments,
    ) -> Result<(), FederationError> {
        if fragments.is_empty() {
            return Ok(());
        }

        // Calling optimize() will not match a fragment that would have expanded at
        // top-level. That is, say we have the selection set `{ x y }` for a top-level `Query`, and
        // we have a fragment
        // ```
        // fragment F on Query {
        //   x
        //   y
        // }
        // ```
        // then calling `self.optimize(fragments)` would only apply check if F apply to
        // `x` and then `y`.
        //
        // To ensure the fragment match in this case, we "wrap" the selection into a trivial
        // fragment of the selection parent, so in the example above, we create selection `... on
        // Query { x y}`. With that, `optimize` will correctly match on the `on Query`
        // fragment; after which we can unpack the final result.
        let wrapped = InlineFragmentSelection::from_selection_set(
            self.type_position.clone(), // parent type
            self.clone(),               // selection set
            Default::default(),         // directives
        );
        let mut validator = FieldsConflictMultiBranchValidator::from_initial_validator(
            FieldsConflictValidator::from_selection_set(self),
        );
        let optimized = wrapped.optimize(fragments, &mut validator)?;

        // Now, it's possible we matched a full fragment, in which case `optimized` will be just
        // the named fragment, and in that case we return a singleton selection with just that.
        // Otherwise, it's our wrapping inline fragment with the sub-selections optimized, and we
        // just return that subselection.
        match optimized {
            InlineOrFragmentSelection::FragmentSpread(_) => {
                let self_selections = Arc::make_mut(&mut self.selections);
                self_selections.clear();
                self_selections.insert(optimized.into());
            }

            InlineOrFragmentSelection::InlineFragment(inline_fragment) => {
                // Note: `inline_fragment.selection_set` can't be moved (since it's inside Arc).
                // So, it's cloned.
                *self = inline_fragment.selection_set.clone();
            }
        }
        Ok(())
    }
}

impl Operation {
    // PORT_NOTE: The JS version of `optimize` takes an optional `minUsagesToOptimize` argument.
    //            However, it's only used in tests. So, it's removed in the Rust version.
    const DEFAULT_MIN_USAGES_TO_OPTIMIZE: u32 = 2;

    pub(crate) fn optimize(&mut self, fragments: &NamedFragments) -> Result<(), FederationError> {
        if fragments.is_empty() {
            return Ok(());
        }

        // Optimize the operation's selection set by re-using existing fragments.
        let before_optimization = self.selection_set.clone();
        self.selection_set.optimize_at_root(fragments)?;
        if before_optimization == self.selection_set {
            return Ok(());
        }

        // Optimize the named fragment definitions by dropping low-usage ones.
        let mut final_fragments = fragments.clone();
        let final_selection_set =
            final_fragments.reduce(&self.selection_set, Self::DEFAULT_MIN_USAGES_TO_OPTIMIZE)?;

        self.selection_set = final_selection_set;
        self.named_fragments = final_fragments;
        Ok(())
    }

    // Mainly for testing.
    fn expand_all_fragments(&self) -> Result<Self, FederationError> {
        let selection_set = self.selection_set.expand_all_fragments()?;
        Ok(Self {
            named_fragments: Default::default(),
            selection_set,
            ..self.clone()
        })
    }
}

//=============================================================================
// Tests

#[cfg(test)]
mod tests {
    use apollo_compiler::schema::Schema;

    use super::*;
    use crate::schema::ValidFederationSchema;

    fn parse_schema(schema_doc: &str) -> ValidFederationSchema {
        let schema = Schema::parse_and_validate(schema_doc, "schema.graphql").unwrap();
        ValidFederationSchema::new(schema).unwrap()
    }

    fn parse_operation(schema: &ValidFederationSchema, query: &str) -> Operation {
        let executable_document = apollo_compiler::ExecutableDocument::parse_and_validate(
            schema.schema(),
            query,
            "query.graphql",
        )
        .unwrap();
        let operation = executable_document.get_operation(None).unwrap();
        let named_fragments = NamedFragments::new(&executable_document.fragments, schema);
        let selection_set =
            SelectionSet::from_selection_set(&operation.selection_set, &named_fragments, schema)
                .unwrap();

        Operation {
            schema: schema.clone(),
            root_kind: operation.operation_type.into(),
            name: operation.name.clone(),
            variables: Arc::new(operation.variables.clone()),
            directives: Arc::new(operation.directives.clone()),
            selection_set,
            named_fragments,
        }
    }

    macro_rules! assert_without_fragments {
        ($operation: expr, @$expected: literal) => {{
            let without_fragments = $operation.expand_all_fragments().unwrap();
            insta::assert_snapshot!(without_fragments, @$expected);
            without_fragments
        }};
    }

    macro_rules! assert_optimized {
        ($operation: expr, $named_fragments: expr, @$expected: literal) => {{
            let mut optimized = $operation.clone();
            optimized.optimize(&$named_fragments).unwrap();
            insta::assert_snapshot!(optimized, @$expected)
        }};
    }

    #[test]
    fn optimize_fragments_using_other_fragments_when_possible() {
        let schema = r#"
              type Query {
                t: I
              }

              interface I {
                b: Int
                u: U
              }

              type T1 implements I {
                a: Int
                b: Int
                u: U
              }

              type T2 implements I {
                x: String
                y: String
                b: Int
                u: U
              }

              union U = T1 | T2
        "#;

        let query = r#"
              fragment OnT1 on T1 {
                a
                b
              }

              fragment OnT2 on T2 {
                x
                y
              }

              fragment OnI on I {
                b
              }

              fragment OnU on U {
                ...OnI
                ...OnT1
                ...OnT2
              }

              query {
                t {
                  ...OnT1
                  ...OnT2
                  ...OnI
                  u {
                    ...OnU
                  }
                }
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            operation,
            @r###"
        {
          t {
            ... on T1 {
              a
              b
            }
            ... on T2 {
              x
              y
            }
            b
            u {
              ... on I {
                b
              }
              ... on T1 {
                a
                b
              }
              ... on T2 {
                x
                y
              }
            }
          }
        }
        "###
        );

        assert_optimized!(expanded, operation.named_fragments, @r###"
              fragment OnU on U {
                ... on I {
                  b
                }
                ... on T1 {
                  a
                  b
                }
                ... on T2 {
                  x
                  y
                }
              }

              {
                t {
                  ...OnU
                  u {
                    ...OnU
                  }
                }
              }
        "###);
    }

    #[test]
    #[ignore] // Appears to be an expansion bug.
    fn handles_fragments_using_other_fragments() {
        let schema = r#"
              type Query {
                t: I
              }

              interface I {
                b: Int
                c: Int
                u1: U
                u2: U
              }

              type T1 implements I {
                a: Int
                b: Int
                c: Int
                me: T1
                u1: U
                u2: U
              }

              type T2 implements I {
                x: String
                y: String
                b: Int
                c: Int
                u1: U
                u2: U
              }

              union U = T1 | T2
        "#;

        let query = r#"
              fragment OnT1 on T1 {
                a
                b
              }

              fragment OnT2 on T2 {
                x
                y
              }

              fragment OnI on I {
                b
                c
              }

              fragment OnU on U {
                ...OnI
                ...OnT1
                ...OnT2
              }

              query {
                t {
                  ...OnT1
                  ...OnT2
                  u1 {
                    ...OnU
                  }
                  u2 {
                    ...OnU
                  }
                  ... on T1 {
                    me {
                      ...OnI
                    }
                  }
                }
              }
        "#;

        let operation = parse_operation(&parse_schema(schema), query);

        let expanded = assert_without_fragments!(
            &operation,
            @r###"
              {
                t {
                  ... on T1 {
                    a
                    b
                    me {
                      b
                      c
                    }
                  }
                  ... on T2 {
                    x
                    y
                  }
                  u1 {
                    ... on I {
                      b
                      c
                    }
                    ... on T1 {
                      a
                      b
                    }
                    ... on T2 {
                      x
                      y
                    }
                  }
                  u2 {
                    ... on I {
                      b
                      c
                    }
                    ... on T1 {
                      a
                      b
                    }
                    ... on T2 {
                      x
                      y
                    }
                  }
                }
              }
        "###);

        // We should reuse and keep all fragments, because 1) onU is used twice and 2)
        // all the other ones are used once in the query, and once in onU definition.
        assert_optimized!(expanded, operation.named_fragments, @r###"
              fragment OnT1 on T1 {
                a
                b
              }

              fragment OnT2 on T2 {
                x
                y
              }

              fragment OnI on I {
                b
                c
              }

              fragment OnU on U {
                ...OnI
                ...OnT1
                ...OnT2
              }

              {
                t {
                  ... on T1 {
                    ...OnT1
                    me {
                      ...OnI
                    }
                  }
                  ...OnT2
                  u1 {
                    ...OnU
                  }
                  u2 {
                    ...OnU
                  }
                }
              }
        "###);
    }

    macro_rules! test_fragments_roundtrip {
        ($schema_doc: expr, $query: expr, @$expanded: literal) => {{
            let schema = parse_schema($schema_doc);
            let operation = parse_operation(&schema, $query);
            let without_fragments = operation.expand_all_fragments().unwrap();
            insta::assert_snapshot!(without_fragments, @$expanded);

            let mut optimized = without_fragments;
            optimized.optimize(&operation.named_fragments).unwrap();
            assert_eq!(optimized.to_string(), operation.to_string());
        }};
    }

    #[test]
    fn handles_fragments_with_nested_selections() {
        let schema_doc = r#"
              type Query {
                t1a: T1
                t2a: T1
              }

              type T1 {
                t2: T2
              }

              type T2 {
                x: String
                y: String
              }
        "#;

        let query = r#"
                fragment OnT1 on T1 {
                  t2 {
                    x
                  }
                }

                query {
                  t1a {
                    ...OnT1
                    t2 {
                      y
                    }
                  }
                  t2a {
                    ...OnT1
                  }
                }
        "#;

        test_fragments_roundtrip!(schema_doc, query, @r###"
                {
                  t1a {
                    t2 {
                      x
                      y
                    }
                  }
                  t2a {
                    t2 {
                      x
                    }
                  }
                }
        "###);
    }
}
