use std::sync::Arc;

use apollo_compiler::executable;
use apollo_compiler::name;
use apollo_compiler::Node;

use super::DirectiveList;
use super::runtime_types_intersect;
use super::Field;
use super::FieldData;
use super::FieldSelection;
use super::FragmentSpreadSelection;
use super::InlineFragmentSelection;
use super::NamedFragments;
use super::Selection;
use super::SelectionMap;
use super::SelectionSet;
use crate::error::FederationError;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;

#[derive(Debug, Clone, PartialEq, Eq, derive_more::From)]
pub(crate) enum SelectionOrSet {
    Selection(Selection),
    SelectionSet(SelectionSet),
}

impl Selection {
    fn flatten_unnecessary_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        match self {
            Selection::Field(field) => {
                field.flatten_unnecessary_fragments(parent_type, named_fragments, schema)
            }
            Selection::FragmentSpread(spread) => {
                spread.flatten_unnecessary_fragments(parent_type, named_fragments, schema)
            }
            Selection::InlineFragment(inline) => {
                inline.flatten_unnecessary_fragments(parent_type, named_fragments, schema)
            }
        }
    }
}

impl FieldSelection {
    fn flatten_unnecessary_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        let field_position =
            if self.field.schema() == schema && self.field.parent_type_position() == *parent_type {
                self.field.field_position.clone()
            } else {
                parent_type.field(self.field.name().clone())?
            };

        let field_element =
            if self.field.schema() == schema && self.field.field_position == field_position {
                self.field.data().clone()
            } else {
                self.field
                    .with_updated_position(schema.clone(), field_position)
            };

        if let Some(selection_set) = &self.selection_set {
            let field_composite_type_position: CompositeTypeDefinitionPosition =
                field_element.output_base_type()?.try_into()?;
            let mut normalized_selection: SelectionSet = selection_set
                .flatten_unnecessary_fragments(
                    &field_composite_type_position,
                    named_fragments,
                    schema,
                )?;

            let mut selection = self.with_updated_element(field_element);
            if normalized_selection.is_empty() {
                // In rare cases, it's possible that everything in the sub-selection was trimmed away and so the
                // sub-selection is empty. Which suggest something may be wrong with this part of the query
                // intent, but the query was valid while keeping an empty sub-selection isn't. So in that
                // case, we just add some "non-included" __typename field just to keep the query valid.
                let directives = DirectiveList::one(executable::Directive {
                    name: name!("include"),
                    arguments: vec![(name!("if"), false).into()],
                });
                let non_included_typename = Selection::from_field(
                    Field::new(FieldData {
                        schema: schema.clone(),
                        field_position: field_composite_type_position
                            .introspection_typename_field(),
                        alias: None,
                        arguments: Arc::new(vec![]),
                        directives,
                        sibling_typename: None,
                    }),
                    None,
                );
                let mut typename_selection = SelectionMap::new();
                typename_selection.insert(non_included_typename);

                normalized_selection.selections = Arc::new(typename_selection);
                selection.selection_set = Some(normalized_selection);
            } else {
                selection.selection_set = Some(normalized_selection);
            }
            Ok(Some(SelectionOrSet::Selection(Selection::from(selection))))
        } else {
            Ok(Some(SelectionOrSet::Selection(Selection::from(
                self.with_updated_element(field_element),
            ))))
        }
    }
}

impl FragmentSpreadSelection {
    fn flatten_unnecessary_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        let this_condition = self.spread.type_condition_position.clone();
        // This method assumes by contract that `parent_type` runtimes intersects `self.inline_fragment.parent_type_position`'s,
        // but `parent_type` runtimes may be a subset. So first check if the selection should not be discarded on that account (that
        // is, we should not keep the selection if its condition runtimes don't intersect at all with those of
        // `parent_type` as that would ultimately make an invalid selection set).
        if (self.spread.schema != *schema || this_condition != *parent_type)
            && !runtime_types_intersect(&this_condition, parent_type, schema)
        {
            return Ok(None);
        }

        // We must update the spread parent type if necessary since we're not going deeper,
        // or we'll be fundamentally losing context.
        if self.spread.schema != *schema {
            return Err(FederationError::internal(
                "Should not try to flatten_unnecessary_fragments using a type from another schema",
            ));
        }

        let rebased_fragment_spread = self.rebase_on(parent_type, named_fragments, schema)?;
        Ok(Some(SelectionOrSet::Selection(rebased_fragment_spread)))
    }
}

impl InlineFragmentSelection {
    fn flatten_unnecessary_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<Option<SelectionOrSet>, FederationError> {
        let this_condition = self.inline_fragment.type_condition_position.as_ref();
        // This method assumes by contract that `parent_type` runtimes intersects `self.inline_fragment.parent_type_position`'s,
        // but `parent_type` runtimes may be a subset. So first check if the selection should not be discarded on that account (that
        // is, we should not keep the selection if its condition runtimes don't intersect at all with those of
        // `parent_type` as that would ultimately make an invalid selection set).
        if let Some(type_condition) = this_condition {
            if (self.inline_fragment.schema != *schema
                || self.inline_fragment.parent_type_position != *parent_type)
                && !runtime_types_intersect(type_condition, parent_type, schema)
            {
                return Ok(None);
            }
        }

        // We know the condition is "valid", but it may not be useful. That said, if the condition has directives,
        // we preserve the fragment no matter what.
        if self.inline_fragment.directives.is_empty() {
            // There is a number of cases where a fragment is not useful:
            // 1. if there is no type condition (remember it also has no directives).
            // 2. if it's the same type as the current type: it's not restricting types further.
            // 3. if the current type is an object more generally: because in that case the condition
            //   cannot be restricting things further (it's typically a less precise interface/union).
            let useless_fragment = match this_condition {
                None => true,
                Some(c) => self.inline_fragment.schema == *schema && c == parent_type,
            };
            if useless_fragment || parent_type.is_object_type() {
                // Try to skip this fragment and flatten_unnecessary_fragments self.selection_set with `parent_type`,
                // instead of its original type.
                let selection_set = self.selection_set.flatten_unnecessary_fragments(
                    parent_type,
                    named_fragments,
                    schema,
                )?;
                return if selection_set.is_empty() {
                    Ok(None)
                } else {
                    // We need to rebase since the parent type for the selection set could be
                    // changed.
                    // Note: Rebasing after flattening, since rebasing before that can error out.
                    //       Or, `flatten_unnecessary_fragments` could `rebase` at the same time.
                    let selection_set = if useless_fragment {
                        selection_set.clone()
                    } else {
                        selection_set.rebase_on(parent_type, named_fragments, schema)?
                    };
                    Ok(Some(SelectionOrSet::SelectionSet(selection_set)))
                };
            }
        }

        // Note: This selection_set is not rebased here yet. It will be rebased later as necessary.
        let selection_set = self.selection_set.flatten_unnecessary_fragments(
            &self.selection_set.type_position,
            named_fragments,
            &self.selection_set.schema,
        )?;
        // It could be that nothing was satisfiable.
        if selection_set.is_empty() {
            if self.inline_fragment.directives.is_empty() {
                return Ok(None);
            } else {
                let rebased_fragment = self.inline_fragment.rebase_on(parent_type, schema)?;
                // We should be able to rebase, or there is a bug, so error if that is the case.
                // If we rebased successfully then we add "non-included" __typename field selection
                // just to keep the query valid.
                let directives = DirectiveList::one(executable::Directive {
                    name: name!("include"),
                    arguments: vec![(name!("if"), false).into()],
                });
                let parent_typename_field = if let Some(condition) = this_condition {
                    condition.introspection_typename_field()
                } else {
                    parent_type.introspection_typename_field()
                };
                let typename_field_selection = Selection::from_field(
                    Field::new(FieldData {
                        schema: schema.clone(),
                        field_position: parent_typename_field,
                        alias: None,
                        arguments: Arc::new(vec![]),
                        directives,
                        sibling_typename: None,
                    }),
                    None,
                );

                // Return `... [on <rebased condition>] { __typename @include(if: false) }`
                let rebased_casted_type = rebased_fragment.casted_type();
                return Ok(Some(SelectionOrSet::Selection(
                    InlineFragmentSelection::new(
                        rebased_fragment,
                        SelectionSet::from_selection(rebased_casted_type, typename_field_selection),
                    )
                    .into(),
                )));
            }
        }

        // Second, we check if some of the sub-selection fragments can be "lifted" outside of this fragment. This can happen if:
        // 1. the current fragment is an abstract type,
        // 2. the sub-fragment is an object type,
        // 3. the sub-fragment type is a valid runtime of the current type.
        if self.inline_fragment.directives.is_empty()
            && this_condition.is_some_and(|c| c.is_abstract_type())
        {
            let mut liftable_selections = SelectionMap::new();
            for (_, selection) in selection_set.selections.iter() {
                match selection {
                    Selection::FragmentSpread(spread_selection) => {
                        let type_condition =
                            spread_selection.spread.type_condition_position.clone();
                        if type_condition.is_object_type()
                            && runtime_types_intersect(parent_type, &type_condition, schema)
                        {
                            liftable_selections
                                .insert(Selection::FragmentSpread(spread_selection.clone()));
                        }
                    }
                    Selection::InlineFragment(inline_fragment_selection) => {
                        if let Some(type_condition) = inline_fragment_selection
                            .inline_fragment
                            .type_condition_position
                            .clone()
                        {
                            if type_condition.is_object_type()
                                && runtime_types_intersect(parent_type, &type_condition, schema)
                            {
                                liftable_selections.insert(Selection::InlineFragment(
                                    inline_fragment_selection.clone(),
                                ));
                            }
                        };
                    }
                    _ => continue,
                }
            }

            // If we can lift all selections, then that just mean we can get rid of the current fragment altogether
            if liftable_selections.len() == selection_set.selections.len() {
                // Rebasing is necessary since this normalized sub-selection set changed its parent.
                let rebased_selection_set =
                    selection_set.rebase_on(parent_type, named_fragments, schema)?;
                return Ok(Some(SelectionOrSet::SelectionSet(rebased_selection_set)));
            }

            // Otherwise, if there are "liftable" selections, we must return a set comprised of those lifted selection,
            // and the current fragment _without_ those lifted selections.
            if liftable_selections.len() > 0 {
                // Converting `... [on T] { <liftable_selections> <non-liftable_selections> }` into
                // `{ ... [on T] { <non-liftable_selections> } <liftable_selections> }`.
                // PORT_NOTE: It appears that this lifting could be repeatable (meaning lifted
                // selection could be broken down further and lifted again), but
                // flatten_unnecessary_fragments is not
                // applied recursively. This could be worth investigating.
                let rebased_inline_fragment =
                    self.inline_fragment.rebase_on(parent_type, schema)?;
                let mut mutable_selections = self.selection_set.selections.clone();
                let final_fragment_selections = Arc::make_mut(&mut mutable_selections);
                final_fragment_selections.retain(|k, _| !liftable_selections.contains_key(k));
                let rebased_casted_type = rebased_inline_fragment.casted_type();
                let final_inline_fragment: Selection = InlineFragmentSelection::new(
                    rebased_inline_fragment,
                    SelectionSet {
                        schema: schema.clone(),
                        type_position: rebased_casted_type,
                        selections: Arc::new(final_fragment_selections.clone()),
                    },
                )
                .into();

                // Since liftable_selections are changing their parent, we need to rebase them.
                liftable_selections = liftable_selections
                    .into_iter()
                    .map(|(_key, sel)| sel.rebase_on(parent_type, named_fragments, schema))
                    .collect::<Result<_, _>>()?;

                let mut final_selection_map = SelectionMap::new();
                final_selection_map.insert(final_inline_fragment);
                final_selection_map.extend(liftable_selections);
                let final_selections = SelectionSet {
                    schema: schema.clone(),
                    type_position: parent_type.clone(),
                    selections: final_selection_map.into(),
                };
                return Ok(Some(SelectionOrSet::SelectionSet(final_selections)));
            }
        }

        if self.inline_fragment.schema == *schema
            && self.inline_fragment.parent_type_position == *parent_type
            && self.selection_set == selection_set
        {
            // flattening did not change the fragment
            // TODO(@goto-bus-stop): no change, but we still create a non-trivial clone here
            Ok(Some(SelectionOrSet::Selection(Selection::InlineFragment(
                Arc::new(self.clone()),
            ))))
        } else {
            let rebased_inline_fragment = self.inline_fragment.rebase_on(parent_type, schema)?;
            let rebased_casted_type = rebased_inline_fragment.casted_type();
            let rebased_selection_set =
                selection_set.rebase_on(&rebased_casted_type, named_fragments, schema)?;
            Ok(Some(SelectionOrSet::Selection(Selection::InlineFragment(
                Arc::new(InlineFragmentSelection::new(
                    rebased_inline_fragment,
                    rebased_selection_set,
                )),
            ))))
        }
    }
}

impl SelectionSet {
    /// Simplify this selection set in the context of the provided `parent_type`.
    ///
    /// This removes unnecessary/redundant inline fragments, so that for instance, with a schema:
    /// ```graphql
    /// type Query {
    ///   t1: T1
    ///   i: I
    /// }
    ///
    /// interface I {
    ///   id: ID!
    /// }
    ///
    /// type T1 implements I {
    ///   id: ID!
    ///   v1: Int
    /// }
    ///
    /// type T2 implements I {
    ///   id: ID!
    ///   v2: Int
    /// }
    /// ```
    /// We can perform following simplification:
    /// ```graphql
    /// flatten_unnecessary_fragments({
    ///   t1 {
    ///     ... on I {
    ///       id
    ///     }
    ///   }
    ///   i {
    ///     ... on T1 {
    ///       ... on I {
    ///         ... on T1 {
    ///           v1
    ///         }
    ///         ... on T2 {
    ///           v2
    ///         }
    ///       }
    ///     }
    ///     ... on T2 {
    ///       ... on I {
    ///         id
    ///       }
    ///     }
    ///   }
    /// }) === {
    ///   t1 {
    ///     id
    ///   }
    ///   i {
    ///     ... on T1 {
    ///       v1
    ///     }
    ///     ... on T2 {
    ///       id
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// For this operation to be valid (to not throw), `parent_type` must be such that every field selection in
    /// this selection set is such that its type position intersects with passed `parent_type` (there is no limitation
    /// on the fragment selections, though any fragment selections whose condition do not intersects `parent_type`
    /// will be discarded). Note that `self.flatten_unnecessary_fragments(self.type_condition)` is always valid and useful, but it is
    /// also possible to pass a `parent_type` that is more "restrictive" than the selection current type position
    /// (as long as the top-level fields of this selection set can be rebased on that type).
    ///
    // PORT_NOTE: this is now module-private, because it looks like it *can* be. If some place
    // outside this module *does* need it, feel free to mark it pub(crate).
    // PORT_NOTE: in JS, this was called "normalize".
    // PORT_NOTE: in JS, this had a `recursive: false` flag, which would only apply the
    // simplification at the top level. This appears to be unused.
    pub(super) fn flatten_unnecessary_fragments(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
    ) -> Result<SelectionSet, FederationError> {
        let mut normalized_selections = Self {
            schema: schema.clone(),
            type_position: parent_type.clone(),
            selections: Default::default(), // start empty
        };
        for selection in self.selections.values() {
            if let Some(selection_or_set) =
                selection.flatten_unnecessary_fragments(parent_type, named_fragments, schema)?
            {
                match selection_or_set {
                    SelectionOrSet::Selection(normalized_selection) => {
                        normalized_selections.add_local_selection(&normalized_selection)?;
                    }
                    SelectionOrSet::SelectionSet(normalized_set) => {
                        // Since the `selection` has been expanded/lifted, we use
                        // `add_selection_set_with_fragments` to make sure it's rebased.
                        normalized_selections
                            .add_selection_set_with_fragments(&normalized_set, named_fragments)?;
                    }
                }
            }
        }
        Ok(normalized_selections)
    }
}
