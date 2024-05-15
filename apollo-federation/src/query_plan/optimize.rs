use apollo_compiler::executable::Name;
use apollo_compiler::Node;

use crate::error::FederationError;
use crate::schema::position::CompositeTypeDefinitionPosition;

use super::operation::Containment;
use super::operation::ContainmentOptions;
use super::operation::Fragment;
use super::operation::FragmentSpread;
use super::operation::FragmentSpreadData;
use super::operation::FragmentSpreadSelection;
use super::operation::NamedFragments;
use super::operation::NormalizeSelectionOption;
use super::operation::Selection;
use super::operation::SelectionId;
use super::operation::SelectionKey;
use super::operation::SelectionMap;
use super::operation::SelectionOrSet;
use super::operation::SelectionSet;

struct FieldsConflictValidator {}

struct FieldsConflictMultiBranchValidator {}

#[derive(Clone)]
struct FragmentRestrictionAtType {
    selections: SelectionMap,
    //validator: Option<FieldsConflictValidator>,
}

impl FragmentRestrictionAtType {
    fn new(selections: SelectionMap) -> Self {
        Self { selections }
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
        match self.selections.as_slice().split_first() {
            None => true,

            Some((first, rest)) => rest.is_empty() && first.0.is_typename_field(),
        }
    }
}

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

    // PORT_NOTE: The JS version memoizes the result of this function. But, the current Rust port does not.
    // TODO: consider memoize this function.
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
            // TODO: add validator
            return Ok(FragmentRestrictionAtType::new(
                expanded_selection_set.selections.as_ref().clone(),
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
        let trimmed = expanded_selection_set
            .selections
            .minus(&normalized_selection_set.selections)?;
        // TODO: add validator
        Ok(FragmentRestrictionAtType::new(trimmed))
    }

    // Whether this fragment fully includes `other_fragment`.
    // Note that this is slightly different from `self` "using" `other_fragment` in that this
    // essentially checks if the full selection set of `other_fragment` is contained by `self`, so
    // this only look at "top-level" usages.
    //
    // Note that this is guaranteed to return `false` if passed self's name.
    // PORT_NOTE: The JS version memoizes the result of this function. But, the current Rust port does not.
    // TODO: consider memoize this function.
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

impl Selection {
    // PORT_NOTE: The definition of `minus` and `intersection` functions when either `self` or
    // `other` has no sub-selection seems unintuitive. Why are `apple.minus(orange) = None` and
    // `apple.intersection(orange) = apple`?

    /// Performs set-subtraction (self - other) and returns the result (the difference between self
    /// and other).
    /// If there are respective sub-selections, then we compute their diffs and add them (if not
    /// empty). Otherwise, we have no diff.
    fn minus(&self, other: &Selection) -> Result<Option<Selection>, FederationError> {
        if let (Some(self_sub_selection), Some(other_sub_selection)) =
            (self.selection_set()?, other.selection_set()?)
        {
            let diff = self_sub_selection
                .selections
                .minus(&other_sub_selection.selections)?;
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

    // If there are respective sub-selections, then we compute their intersections and add them
    // (if not empty). Otherwise, the intersection is same as `self`.
    fn intersection(&self, other: &Selection) -> Result<Option<Selection>, FederationError> {
        if let (Some(self_sub_selection), Some(other_sub_selection)) =
            (self.selection_set()?, other.selection_set()?)
        {
            let common = self_sub_selection
                .selections
                .intersection(&other_sub_selection.selections)?;
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

impl SelectionMap {
    /// Performs set-subtraction (self - other) and returns the result (the difference between self
    /// and other).
    fn minus(&self, other: &SelectionMap) -> Result<SelectionMap, FederationError> {
        let iter = self
            .iter()
            .map(|(k, v)| {
                if let Some(other_v) = other.get(k) {
                    v.minus(other_v)
                } else {
                    Ok(Some(v.clone()))
                }
            })
            .collect::<Result<Vec<_>, _>>()? // early break in case of Err
            .into_iter()
            .flatten();
        Ok(SelectionMap::from_iter(iter))
    }

    fn intersection(&self, other: &SelectionMap) -> Result<SelectionMap, FederationError> {
        if self.is_empty() {
            return Ok(self.clone());
        }
        if other.is_empty() {
            return Ok(other.clone());
        }

        let iter = self
            .iter()
            .map(|(k, v)| {
                if let Some(other_v) = other.get(k) {
                    v.intersection(other_v)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<Vec<_>, _>>()? // early break in case of Err
            .into_iter()
            .flatten();
        Ok(SelectionMap::from_iter(iter))
    }
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
        applicable_fragments: &[(Node<Fragment>, FragmentRestrictionAtType)],
    ) -> Vec<(Node<Fragment>, FragmentRestrictionAtType)> {
        // Note: `applicable_fragments.retain()` (and mutably borrowing it) won't work here, since
        //       the filter argument closure also need to borrow `applicable_fragments`.
        applicable_fragments
            .iter()
            .filter(|(fragment, _)| {
                !applicable_fragments
                    .iter()
                    .any(|(other_fragment, _)| other_fragment.includes(&fragment.name))
            })
            .cloned()
            .collect()
    }

    // PORT_NOTE: Moved from `Selection` class in JS code to SelectionSet struct in Rust.
    fn try_optimize_with_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        fragments: &NamedFragments,
        // validator: &SelectionSetValidator,
        // can_use_full_matching_fragment,
    ) -> Result<SelectionOrSet, FederationError> {
        // We limit to fragments whose selection could be applied "directly" at `parent_type`,
        // meaning without taking the fragment condition into account. The idea being that if the
        // fragment condition would be needed inside `parent_type`, then that condition will not
        // have been "normalized away" and so we want for this very call to be called on the
        // fragment whose type _is_ the fragment condition (at which point, this
        // `can_apply_directly_at_type` method will apply. Also note that this is because we have
        // this restriction that calling `expanded_selection_set_at_type` is ok.
        let candidates = fragments.get_all_may_apply_directly_at_type(parent_type)?;
        if candidates.is_empty() {
            return Ok(self.clone().into());
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
            let res = self.selections.containment(
                &at_type.selections,
                ContainmentOptions {
                    ignore_missing_typename: true,
                },
            );
            if matches!(res, Containment::Equal) {
                // NOCHECKIN
                // if can_use_full_matching_fragment(candidate) {
                //     if !validator.check_can_reuse_fragment_and_track_it(at_type) {
                //         // We cannot use it at all, so no point in adding to `applyingFragments`.
                //         continue;
                //     }
                //     return candidate;
                // }

                // If we're not going to replace the full thing, then same reasoning a below.
                if candidate.directives.is_empty() {
                    applicable_fragments.push((candidate, at_type));
                }
            }
            // Note that if a fragment applies to only a subset of the subSelection, then we really only can use
            // it if that fragment is defined _without_ directives.
            else if matches!(res, Containment::StrictlyContained)
                && candidate.directives.is_empty()
            {
                applicable_fragments.push((candidate, at_type));
            }
        }

        if applicable_fragments.is_empty() {
            return Ok(self.clone().into());
        }

        applicable_fragments = Self::reduce_applicable_fragments(&applicable_fragments);

        let mut not_covered_so_far = self.selections.as_ref().clone();
        let mut optimized = SelectionMap::new();
        for (fragment, at_type) in applicable_fragments {
            // TODO: add validator check
            // if !validator.check_can_reuse_fragment_and_track_it(at_type) { continue; }

            let not_covered = self.selections.minus(&at_type.selections)?;
            not_covered_so_far = not_covered_so_far.intersection(&not_covered)?;
            // PORT_NOTE: JS version doesn't do this, but shouldn't we skip such fragments that
            //            don't cover any selections (thus, not reducing `not_covered_so_far`)?

            let fragment_spread_data = FragmentSpreadData {
                schema: self.schema.clone(),
                fragment_name: fragment.name.clone(),
                type_condition_position: parent_type.clone(),
                directives: Default::default(), // No directives added to the spread
                fragment_directives: fragment.directives.clone(), // Directives from the fragment definition
                selection_id: SelectionId::new(),
            };
            let fragment_selection = FragmentSpreadSelection {
                spread: FragmentSpread::new(fragment_spread_data),
                selection_set: fragment.selection_set.clone(),
            };
            optimized.insert(fragment_selection.into());
        }

        optimized.extend_ref(&not_covered_so_far);
        Ok(SelectionSet::make_selection_set(
            &self.schema,
            parent_type,
            optimized.values().map(std::iter::once),
            fragments,
        )?
        .into())
    }
}
