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
//! ## Matching fragments with selection set (`try_optimize_with_fragments`)
//! This is one strategy to optimize the selection set. It tries to match all applicable fragments.
//! Then, they are expanded into selection sets in order to match them against given selection set.
//! Set-intersection/-minus/-containment operations are used to narrow down to fewer number of
//! fragments that can be used to optimize the selection set. If there is a single fragment that
//! covers the full selection set, then that fragment is used. Otherwise, we attempted to reduce
//! the number of fragments applied, but optimality is not guaranteed, yet.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Not;
use std::sync::Arc;

use apollo_compiler::executable::Name;
use apollo_compiler::Node;

use super::operation::CollectedFieldInSet;
use super::operation::Containment;
use super::operation::ContainmentOptions;
use super::operation::Field;
use super::operation::Fragment;
use super::operation::FragmentSpreadSelection;
use super::operation::NamedFragments;
use super::operation::NormalizeSelectionOption;
use super::operation::Selection;
use super::operation::SelectionKey;
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
    fn minus(&self, other: &SelectionSet) -> Result<SelectionSet, FederationError> {
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
// Filtering applicable fragments

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

        // Short-circuit #2: The type condition is a (different) object type (too restrictive).
        // - It will never cover all of the runtime types of `ty` unless it's the same type, which is
        //   already checked.
        if self.type_condition_position.is_object_type() {
            return Ok(false);
        }

        // Short-circuit #3: The type condition is an interface type, but the `ty` is more general.
        // - The type condition is an interface but `ty` is a (different) interface or a union.
        if self.type_condition_position.is_interface_type() && !ty.is_object_type() {
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
                // we've seen a previous version of `field` before, we know it's guarantee to have
                // had no selection set here, either. So the `set` below may overwrite a previous
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

        if validator.do_merge_with_all(self.validators.iter().map(Arc::as_ref))? {
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

/// The return type for `SelectionSet::try_optimize_with_fragments`.
#[derive(derive_more::From)]
enum SelectionSetOrFragment {
    //Selection(Selection),
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
        can_use_full_matching_fragment: impl Fn(&Fragment) -> bool,
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
            if matches!(res, Containment::Equal) && can_use_full_matching_fragment(&candidate) {
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
