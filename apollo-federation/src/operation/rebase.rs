use std::sync::Arc;

use super::runtime_types_intersect;
use super::Field;
use super::FieldSelection;
use super::Fragment;
use super::FragmentSpread;
use super::FragmentSpreadData;
use super::FragmentSpreadSelection;
use super::InlineFragment;
use super::InlineFragmentData;
use super::InlineFragmentSelection;
use super::NamedFragments;
use super::NormalizeSelectionOption;
use super::OperationElement;
use super::Selection;
use super::SelectionId;
use super::SelectionSet;
use super::TYPENAME_FIELD;
use crate::error::FederationError;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;

// TODO(@goto-bus-stop): this is precomputed in the QueryPlanner constructor. Can we expose that
// here? Or can we move it onto the FederationSchema instance?
fn is_interface_object(obj: &ObjectTypeDefinitionPosition, schema: &ValidFederationSchema) -> bool {
    if let Ok(intf_obj_directive) = get_federation_spec_definition_from_subgraph(schema)
        .and_then(|spec| spec.interface_object_directive(schema))
    {
        obj.try_get(schema.schema())
            .is_some_and(|o| o.directives.has(&intf_obj_directive.name))
    } else {
        false
    }
}

fn print_possible_runtimes(
    composite_type: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> String {
    schema
        .possible_runtime_types(composite_type.clone())
        .map_or_else(
            |_| "undefined".to_string(),
            |runtimes| {
                runtimes
                    .iter()
                    .map(|r| r.type_name.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            },
        )
}

/// Options for handling rebasing errors.
#[derive(Clone, Copy)]
pub enum RebaseErrorHandlingOption {
    IgnoreError,
    ThrowError,
}

impl Selection {
    pub fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        match self {
            Selection::Field(field) => {
                field.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
            Selection::FragmentSpread(spread) => {
                spread.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
            Selection::InlineFragment(inline) => {
                inline.rebase_on(parent_type, named_fragments, schema, error_handling)
            }
        }
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        match self {
            Selection::Field(field) => field.can_add_to(parent_type, schema),
            // Since `rebaseOn` never fails, we copy the logic here and always return `true`. But as
            // mentioned in `rebaseOn`, this leaves it a bit to the caller to know what they're
            // doing.
            Selection::FragmentSpread(_) => Ok(true),
            Selection::InlineFragment(inline) => inline.can_add_to(parent_type, schema),
        }
    }
}

impl Field {
    pub fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Field>, FederationError> {
        let field_parent = self.data().field_position.parent();
        if self.data().schema == *schema && field_parent == *parent_type {
            // pointing to the same parent -> return self
            return Ok(Some(self.clone()));
        }

        if self.data().name() == &TYPENAME_FIELD {
            // TODO interface object info should be precomputed in QP constructor
            return if schema
                .possible_runtime_types(parent_type.clone())?
                .iter()
                .any(|t| is_interface_object(t, schema))
            {
                if let RebaseErrorHandlingOption::ThrowError = error_handling {
                    Err(FederationError::internal(
                        format!("Cannot add selection of field \"{}\" to selection set of parent type \"{}\" that is potentially an interface object type at runtime",
                                self.data().field_position,
                                parent_type
                        )))
                } else {
                    Ok(None)
                }
            } else {
                let mut updated_field_data = self.data().clone();
                updated_field_data.schema = schema.clone();
                updated_field_data.field_position = parent_type.introspection_typename_field();
                Ok(Some(Field::new(updated_field_data)))
            };
        }

        let field_from_parent = parent_type.field(self.data().name().clone())?;
        return if field_from_parent.try_get(schema.schema()).is_some()
            && self.can_rebase_on(parent_type)
        {
            let mut updated_field_data = self.data().clone();
            updated_field_data.schema = schema.clone();
            updated_field_data.field_position = field_from_parent;
            Ok(Some(Field::new(updated_field_data)))
        } else if let RebaseErrorHandlingOption::IgnoreError = error_handling {
            Ok(None)
        } else {
            Err(FederationError::internal(format!(
                "Cannot add selection of field \"{}\" to selection set of parent type \"{}\"",
                self.data().field_position,
                parent_type
            )))
        };
    }

    /// Verifies whether given field can be rebase on following parent type.
    ///
    /// There are 2 valid cases we want to allow:
    /// 1. either `parent_type` and `field_parent_type` are the same underlying type (same name) but from different underlying schema. Typically,
    ///  happens when we're building subgraph queries but using selections from the original query which is against the supergraph API schema.
    /// 2. or they are not the same underlying type, but the field parent type is from an interface (or an interface object, which is the same
    ///  here), in which case we may be rebasing an interface field on one of the implementation type, which is ok. Note that we don't verify
    ///  that `parent_type` is indeed an implementation of `field_parent_type` because it's possible that this implementation relationship exists
    ///  in the supergraph, but not in any of the subgraph schema involved here. So we just let it be. Not that `rebase_on` will complain anyway
    ///  if the field name simply does not exist in `parent_type`.
    fn can_rebase_on(&self, parent_type: &CompositeTypeDefinitionPosition) -> bool {
        let field_parent_type = self.data().field_position.parent();
        // case 1
        if field_parent_type.type_name() == parent_type.type_name() {
            return true;
        }
        // case 2
        let is_interface_object_type =
            match ObjectTypeDefinitionPosition::try_from(field_parent_type.clone()) {
                Ok(ref o) => is_interface_object(o, &self.data().schema),
                Err(_) => false,
            };
        field_parent_type.is_interface_type() || is_interface_object_type
    }

    fn type_if_added_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Option<OutputTypeDefinitionPosition>, FederationError> {
        let data = self.data();
        if data.field_position.parent() == *parent_type && data.schema == *schema {
            let base_ty_name = data
                .field_position
                .get(schema.schema())?
                .ty
                .inner_named_type();
            return Ok(Some(
                data.schema.get_type(base_ty_name.clone())?.try_into()?,
            ));
        }
        if data.name() == &TYPENAME_FIELD {
            let Some(type_name) = parent_type
                .introspection_typename_field()
                .try_get(schema.schema())
                .map(|field| field.ty.inner_named_type())
            else {
                return Ok(None);
            };
            return Ok(Some(schema.get_type(type_name.clone())?.try_into()?));
        }
        if self.can_rebase_on(parent_type) {
            let Some(type_name) = parent_type
                .field(data.field_position.field_name().clone())
                .ok()
                .and_then(|field_pos| field_pos.get(schema.schema()).ok())
                .map(|field| field.ty.inner_named_type())
            else {
                return Ok(None);
            };
            Ok(Some(schema.get_type(type_name.clone())?.try_into()?))
        } else {
            Ok(None)
        }
    }
}

impl FieldSelection {
    /// Returns a field selection "equivalent" to the one represented by this object, but such that its parent type
    /// is the one provided as argument.
    ///
    /// Obviously, this operation will only succeed if this selection (both the field itself and its subselections)
    /// make sense from the provided parent type. If this is not the case, this method will throw.
    pub fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        if &self.field.data().schema == schema
            && &self.field.data().field_position.parent() == parent_type
        {
            // we are rebasing field on the same parent within the same schema - we can just return self
            return Ok(Some(Selection::from(self.clone())));
        }

        let Some(rebased) = self.field.rebase_on(parent_type, schema, error_handling)? else {
            // rebasing failed but we are ignoring errors
            return Ok(None);
        };

        let Some(selection_set) = &self.selection_set else {
            // leaf field
            return Ok(Some(Selection::from_field(rebased, None)));
        };

        let rebased_type_name = rebased
            .data()
            .field_position
            .get(schema.schema())?
            .ty
            .inner_named_type();
        let rebased_base_type: CompositeTypeDefinitionPosition =
            schema.get_type(rebased_type_name.clone())?.try_into()?;

        let selection_set_type = &selection_set.type_position;
        if self.field.data().schema == rebased.data().schema
            && &rebased_base_type == selection_set_type
        {
            // we are rebasing within the same schema and the same base type
            return Ok(Some(Selection::from_field(
                rebased.clone(),
                self.selection_set.clone(),
            )));
        }

        let rebased_selection_set =
            selection_set.rebase_on(&rebased_base_type, named_fragments, schema, error_handling)?;
        if rebased_selection_set.selections.is_empty() {
            // empty selection set
            Ok(None)
        } else {
            Ok(Some(Selection::from_field(
                rebased.clone(),
                Some(rebased_selection_set),
            )))
        }
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        if self.field.data().schema == *schema
            && self.field.data().field_position.parent() == *parent_type
        {
            return Ok(true);
        }

        let Some(ty) = self.field.type_if_added_to(parent_type, schema)? else {
            return Ok(false);
        };

        if let Some(set) = &self.selection_set {
            let ty: CompositeTypeDefinitionPosition = ty.try_into()?;
            if !(set.schema == *schema && set.type_position == ty) {
                return set.can_rebase_on(&ty, schema);
            }
        }
        Ok(true)
    }
}

impl FragmentSpread {
    /// - `named_fragments`: named fragment definitions that are rebased for the subgraph.
    // Note: Unlike other `rebase_on`, this method should only be used during fetch operation
    //       optimization. Thus, it's rebasing within the same subgraph schema.
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        named_fragments: &NamedFragments,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<FragmentSpread>, FederationError> {
        let Some(named_fragment) = named_fragments.get(&self.data().fragment_name) else {
            return if let RebaseErrorHandlingOption::ThrowError = error_handling {
                Err(FederationError::internal(format!(
                    "Cannot rebase {} fragment if it isn't part of the provided fragments",
                    self.data().fragment_name
                )))
            } else {
                Ok(None)
            };
        };
        debug_assert_eq!(
            *schema,
            self.data().schema,
            "Fragment spread should only be rebased within the same subgraph"
        );
        debug_assert_eq!(
            *schema, named_fragment.schema,
            "Referenced named fragment should've been rebased for the subgraph"
        );
        if !runtime_types_intersect(
            parent_type,
            &named_fragment.type_condition_position,
            &self.data().schema,
        ) {
            return Ok(None);
        }
        Ok(Some(FragmentSpread::new(
            FragmentSpreadData::from_fragment(&named_fragment, &self.data().directives),
        )))
    }
}

impl FragmentSpreadSelection {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        // We preserve the parent type here, to make sure we don't lose context, but we actually don't
        // want to expand the spread as that would compromise the code that optimize subgraph fetches to re-use named
        // fragments.
        //
        // This is a little bit iffy, because the fragment may not apply at this parent type, but we
        // currently leave it to the caller to ensure this is not a mistake. But most of the
        // QP code works on selections with fully expanded fragments, so this code (and that of `can_add_to`
        // on come into play in the code for reusing fragments, and that code calls those methods
        // appropriately.
        if self.spread.data().schema == *schema
            && self.spread.data().type_condition_position == *parent_type
        {
            return Ok(Some(Selection::FragmentSpread(Arc::new(self.clone()))));
        }

        let rebase_on_same_schema = self.spread.data().schema == *schema;
        let Some(named_fragment) = named_fragments.get(&self.spread.data().fragment_name) else {
            // If we're rebasing on another schema (think a subgraph), then named fragments will have been rebased on that, and some
            // of them may not contain anything that is on that subgraph, in which case they will not have been included at all.
            // If so, then as long as we're not asked to error if we cannot rebase, then we're happy to skip that spread (since again,
            // it expands to nothing that applies on the schema).
            return if let RebaseErrorHandlingOption::ThrowError = error_handling {
                Err(FederationError::internal(format!(
                    "Cannot rebase {} fragment if it isn't part of the provided fragments",
                    self.spread.data().fragment_name
                )))
            } else {
                Ok(None)
            };
        };

        // Lastly, if we rebase on a different schema, it's possible the fragment type does not intersect the
        // parent type. For instance, the parent type could be some object type T while the fragment is an
        // interface I, and T may implement I in the supergraph, but not in a particular subgraph (of course,
        // if I doesn't exist at all in the subgraph, then we'll have exited above, but I may exist in the
        // subgraph, just not be implemented by T for some reason). In that case, we can't reuse the fragment
        // as its spread is essentially invalid in that position, so we have to replace it by the expansion
        // of that fragment, which we rebase on the parentType (which in turn, will remove anythings within
        // the fragment selection that needs removing, potentially everything).
        if !rebase_on_same_schema
            && !runtime_types_intersect(
                parent_type,
                &named_fragment.type_condition_position,
                schema,
            )
        {
            // Note that we've used the rebased `named_fragment` to check the type intersection because we needed to
            // compare runtime types "for the schema we're rebasing into". But now that we're deciding to not reuse
            // this rebased fragment, what we rebase is the selection set of the non-rebased fragment. And that's
            // important because the very logic we're hitting here may need to happen inside the rebase on the
            // fragment selection, but that logic would not be triggered if we used the rebased `named_fragment` since
            // `rebase_on_same_schema` would then be 'true'.
            let expanded_selection_set = self.selection_set.rebase_on(
                parent_type,
                named_fragments,
                schema,
                error_handling,
            )?;
            // In theory, we could return the selection set directly, but making `SelectionSet.rebase_on` sometimes
            // return a `SelectionSet` complicate things quite a bit. So instead, we encapsulate the selection set
            // in an "empty" inline fragment. This make for non-really-optimal selection sets in the (relatively
            // rare) case where this is triggered, but in practice this "inefficiency" is removed by future calls
            // to `normalize`.
            return if expanded_selection_set.selections.is_empty() {
                Ok(None)
            } else {
                Ok(Some(
                    InlineFragmentSelection::new(
                        InlineFragment::new(InlineFragmentData {
                            schema: schema.clone(),
                            parent_type_position: parent_type.clone(),
                            type_condition_position: None,
                            directives: Default::default(),
                            selection_id: SelectionId::new(),
                        }),
                        expanded_selection_set,
                    )
                    .into(),
                ))
            };
        }

        let spread = FragmentSpread::new(FragmentSpreadData::from_fragment(
            &named_fragment,
            &self.spread.data().directives,
        ));
        Ok(Some(Selection::FragmentSpread(Arc::new(
            FragmentSpreadSelection {
                spread,
                selection_set: named_fragment.selection_set.clone(),
            },
        ))))
    }
}

impl InlineFragmentData {
    fn casted_type_if_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Option<CompositeTypeDefinitionPosition> {
        if self.schema == *schema && self.parent_type_position == *parent_type {
            return Some(self.casted_type());
        }
        match self.can_rebase_on(parent_type, schema) {
            (false, _) => None,
            (true, None) => Some(parent_type.clone()),
            (true, Some(ty)) => Some(ty),
        }
    }

    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> (bool, Option<CompositeTypeDefinitionPosition>) {
        let Some(ty) = self.type_condition_position.as_ref() else {
            return (true, None);
        };
        match schema
            .get_type(ty.type_name().clone())
            .and_then(CompositeTypeDefinitionPosition::try_from)
        {
            Ok(ty) if runtime_types_intersect(parent_type, &ty, schema) => (true, Some(ty)),
            _ => (false, None),
        }
    }
}

impl InlineFragment {
    pub fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<InlineFragment>, FederationError> {
        if self.data().schema == *schema && self.data().parent_type_position == *parent_type {
            return Ok(Some(self.clone()));
        }

        let type_condition = self.data().type_condition_position.clone();
        // This usually imply that the fragment is not from the same subgraph than the selection. So we need
        // to update the source type of the fragment, but also "rebase" the condition to the selection set
        // schema.
        let (can_rebase, rebased_condition) = self.can_rebase_on(parent_type, schema);
        if !can_rebase {
            if let RebaseErrorHandlingOption::ThrowError = error_handling {
                let printable_type_condition = self
                    .data()
                    .type_condition_position
                    .clone()
                    .map_or_else(|| "".to_string(), |t| t.to_string());
                let printable_runtimes = type_condition.map_or_else(
                    || "undefined".to_string(),
                    |t| print_possible_runtimes(&t, schema),
                );
                let printable_parent_runtimes = print_possible_runtimes(parent_type, schema);
                Err(FederationError::internal(
                    format!("Cannot add fragment of condition \"{}\" (runtimes: [{}]) to parent type \"{}\" (runtimes: [{}])",
                            printable_type_condition,
                            printable_runtimes,
                            parent_type,
                            printable_parent_runtimes,
                    ),
                ))
            } else {
                Ok(None)
            }
        } else {
            let mut rebased_fragment_data = self.data().clone();
            rebased_fragment_data.type_condition_position = rebased_condition;
            rebased_fragment_data.schema = schema.clone();
            Ok(Some(InlineFragment::new(rebased_fragment_data)))
        }
    }

    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        parent_schema: &ValidFederationSchema,
    ) -> (bool, Option<CompositeTypeDefinitionPosition>) {
        if self.data().type_condition_position.is_none() {
            // can_rebase = true, condition = undefined
            return (true, None);
        }

        if let Some(Ok(rebased_condition)) = self
            .data()
            .type_condition_position
            .clone()
            .and_then(|condition_position| {
                parent_schema.try_get_type(condition_position.type_name().clone())
            })
            .map(|rebased_condition_position| {
                CompositeTypeDefinitionPosition::try_from(rebased_condition_position)
            })
        {
            // chained if let chains are not yet supported
            // see https://github.com/rust-lang/rust/issues/53667
            if runtime_types_intersect(parent_type, &rebased_condition, parent_schema) {
                // can_rebase = true, condition = rebased_condition
                (true, Some(rebased_condition))
            } else {
                (false, None)
            }
        } else {
            // can_rebase = false, condition = undefined
            (false, None)
        }
    }
}

impl InlineFragmentSelection {
    pub fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<Option<Selection>, FederationError> {
        if &self.inline_fragment.data().schema == schema
            && self.inline_fragment.data().parent_type_position == *parent_type
        {
            // we are rebasing inline fragment on the same parent within the same schema - we can just return self
            return Ok(Some(Selection::from(self.clone())));
        }

        let Some(rebased_fragment) =
            self.inline_fragment
                .rebase_on(parent_type, schema, error_handling)?
        else {
            // rebasing failed but we are ignoring errors
            return Ok(None);
        };

        let rebased_casted_type = rebased_fragment.data().casted_type();
        if &self.inline_fragment.data().schema == schema
            && self.inline_fragment.data().casted_type() == rebased_casted_type
        {
            // we are within the same schema - selection set does not have to be rebased
            Ok(Some(
                InlineFragmentSelection::new(rebased_fragment, self.selection_set.clone()).into(),
            ))
        } else {
            let rebased_selection_set = self.selection_set.rebase_on(
                &rebased_casted_type,
                named_fragments,
                schema,
                error_handling,
            )?;
            if rebased_selection_set.selections.is_empty() {
                // empty selection set
                Ok(None)
            } else {
                Ok(Some(
                    InlineFragmentSelection::new(rebased_fragment, rebased_selection_set).into(),
                ))
            }
        }
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        if self.inline_fragment.data().schema == *schema
            && self.inline_fragment.data().parent_type_position == *parent_type
        {
            return Ok(true);
        }
        let Some(ty) = self
            .inline_fragment
            .data()
            .casted_type_if_add_to(parent_type, schema)
        else {
            return Ok(false);
        };
        if !(self.selection_set.schema == *schema && self.selection_set.type_position == ty) {
            self.selection_set.can_rebase_on(&ty, schema)
        } else {
            Ok(true)
        }
    }

    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        parent_schema: &ValidFederationSchema,
    ) -> bool {
        self.inline_fragment
            .can_rebase_on(parent_type, parent_schema)
            .0
    }
}

impl OperationElement {
    pub(crate) fn rebase_on_or_error(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        named_fragments: &NamedFragments,
    ) -> Result<OperationElement, FederationError> {
        let result: Option<OperationElement> = match self {
            OperationElement::Field(field) => field
                .rebase_on(parent_type, schema, RebaseErrorHandlingOption::ThrowError)
                .map(|val| val.map(Into::into)),
            OperationElement::FragmentSpread(fragment) => fragment
                .rebase_on(
                    parent_type,
                    schema,
                    named_fragments,
                    RebaseErrorHandlingOption::ThrowError,
                )
                .map(|val| val.map(Into::into)),
            OperationElement::InlineFragment(inline) => inline
                .rebase_on(parent_type, schema, RebaseErrorHandlingOption::ThrowError)
                .map(|val| val.map(Into::into)),
        }?;
        result.ok_or_else(|| {
            FederationError::internal(format!(
                "Cannot rebase operation element {} on {}",
                self, parent_type
            ))
        })
    }

    pub(crate) fn sub_selection_type_position(
        &self,
    ) -> Result<Option<CompositeTypeDefinitionPosition>, FederationError> {
        match self {
            OperationElement::Field(field) => Ok(field.data().output_base_type()?.try_into().ok()),
            OperationElement::FragmentSpread(_) => Ok(None), // No sub-selection set
            OperationElement::InlineFragment(inline) => Ok(Some(inline.data().casted_type())),
        }
    }
}

impl SelectionSet {
    /// Rebase this selection set so it applies to the given schema and type.
    pub fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        named_fragments: &NamedFragments,
        schema: &ValidFederationSchema,
        error_handling: RebaseErrorHandlingOption,
    ) -> Result<SelectionSet, FederationError> {
        let rebased_results = self
            .selections
            .iter()
            .filter_map(|(_, selection)| {
                selection
                    .rebase_on(parent_type, named_fragments, schema, error_handling)
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SelectionSet::from_raw_selections(
            schema.clone(),
            parent_type.clone(),
            rebased_results,
        ))
    }

    /// Returns true if the selection set would select cleanly from the given type in the given
    /// schema.
    pub fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        for selection in self.selections.values() {
            if !selection.can_add_to(parent_type, schema)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl NamedFragments {
    pub(crate) fn rebase_on(
        &self,
        schema: &ValidFederationSchema,
    ) -> Result<NamedFragments, FederationError> {
        let mut rebased_fragments = NamedFragments::default();
        for fragment in self.fragments.values() {
            if let Ok(rebased_type) = schema
                .get_type(fragment.type_condition_position.type_name().clone())
                .and_then(CompositeTypeDefinitionPosition::try_from)
            {
                if let Ok(mut rebased_selection) = fragment.selection_set.rebase_on(
                    &rebased_type,
                    &rebased_fragments,
                    schema,
                    RebaseErrorHandlingOption::IgnoreError,
                ) {
                    // Rebasing can leave some inefficiencies in some case (particularly when a spread has to be "expanded", see `FragmentSpreadSelection.rebaseOn`),
                    // so we do a top-level normalization to keep things clean.
                    rebased_selection = rebased_selection.normalize(
                        &rebased_type,
                        &rebased_fragments,
                        schema,
                        NormalizeSelectionOption::NormalizeRecursively,
                    )?;
                    if NamedFragments::is_selection_set_worth_using(&rebased_selection) {
                        let fragment = Fragment {
                            schema: schema.clone(),
                            name: fragment.name.clone(),
                            type_condition_position: rebased_type.clone(),
                            directives: fragment.directives.clone(),
                            selection_set: rebased_selection,
                        };
                        rebased_fragments.insert(fragment);
                    }
                }
            }
        }
        Ok(rebased_fragments)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use indexmap::IndexSet;

    use crate::operation::normalize_operation;
    use crate::operation::tests::parse_schema_and_operation;
    use crate::operation::tests::parse_subgraph;
    use crate::operation::NamedFragments;
    use crate::schema::position::InterfaceTypeDefinitionPosition;

    #[test]
    fn skips_unknown_fragment_fields() {
        let operation_fragments = r#"
query TestQuery {
  t {
    ...FragOnT
  }
}

fragment FragOnT on T {
  v0
  v1
  v2
  u1 {
    v3
    v4
    v5
  }
  u2 {
    v4
    v5
  }
}

type Query {
  t: T
}

type T {
  v0: Int
  v1: Int
  v2: Int
  u1: U
  u2: U
}

type U {
  v3: Int
  v4: Int
  v5: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  _: Int
}

type T {
  v1: Int
  u1: U
}

type U {
  v3: Int
  v5: Int
}"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnT")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnT").unwrap();

            insta::assert_snapshot!(rebased_fragment, @r###"
                    fragment FragOnT on T {
                      v1
                      u1 {
                        v3
                        v5
                      }
                    }
                "###);
        }
    }

    #[test]
    fn skips_unknown_fragment_on_condition() {
        let operation_fragments = r#"
query TestQuery {
  t {
    ...FragOnT
  }
  u {
    ...FragOnU
  }
}

fragment FragOnT on T {
  x
  y
}

fragment FragOnU on U {
  x
  y
}

type Query {
  t: T
  u: U
}

type T {
  x: Int
  y: Int
}

type U {
  x: Int
  y: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );
        assert_eq!(2, executable_document.fragments.len());

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
  y: Int
}"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnT")));
            assert!(!rebased_fragments.contains(&name!("FragOnU")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnT").unwrap();

            let expected = r#"fragment FragOnT on T {
  x
  y
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn skips_unknown_type_within_fragment() {
        let operation_fragments = r#"
query TestQuery {
  i {
    ...FragOnI
  }
}

fragment FragOnI on I {
  id
  otherId
  ... on T1 {
    x
  }
  ... on T2 {
    y
  }
}

type Query {
  i: I
}

interface I {
  id: ID!
  otherId: ID!
}

type T1 implements I {
  id: ID!
  otherId: ID!
  x: Int
}

type T2 implements I {
  id: ID!
  otherId: ID!
  y: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  i: I
}

interface I {
  id: ID!
}

type T2 implements I {
  id: ID!
  y: Int
}
"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnI")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnI").unwrap();

            let expected = r#"fragment FragOnI on I {
  id
  ... on T2 {
    y
  }
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn skips_typename_on_possible_interface_objects_within_fragment() {
        let operation_fragments = r#"
query TestQuery {
  i {
    ...FragOnI
  }
}

fragment FragOnI on I {
  __typename
  id
  x
}

type Query {
  i: I
}

interface I {
  id: ID!
  x: String!
}

type T implements I {
  id: ID!
  x: String!
}
"#;

        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let mut interface_objects: IndexSet<InterfaceTypeDefinitionPosition> = IndexSet::new();
            interface_objects.insert(InterfaceTypeDefinitionPosition {
                type_name: name!("I"),
            });
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &interface_objects,
            )
            .unwrap();

            let subgraph_schema = r#"extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.5", import: [{ name: "@interfaceObject" }, { name: "@key" }])

directive @link(url: String, as: String, import: [link__Import]) repeatable on SCHEMA

directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

directive @interfaceObject on OBJECT

type Query {
  i: I
}

type I @interfaceObject @key(fields: "id") {
  id: ID!
  x: String!
}

scalar link__Import

scalar federation__FieldSet
"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnI")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnI").unwrap();

            let expected = r#"fragment FragOnI on I {
  id
  x
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn skips_fragments_with_trivial_selections() {
        let operation_fragments = r#"
query TestQuery {
  t {
    ...F1
    ...F2
    ...F3
  }
}

fragment F1 on T {
  a
  b
}

fragment F2 on T {
  __typename
  a
  b
}

fragment F3 on T {
  __typename
  a
  b
  c
  d
}

type Query {
  t: T
}

type T {
  a: Int
  b: Int
  c: Int
  d: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  c: Int
  d: Int
}
"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
            assert_eq!(1, rebased_fragments.size());
            assert!(rebased_fragments.contains(&name!("F3")));
            let rebased_fragment = rebased_fragments.fragments.get("F3").unwrap();

            let expected = r#"fragment F3 on T {
  __typename
  c
  d
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn handles_skipped_fragments_within_fragments() {
        let operation_fragments = r#"
query TestQuery {
  ...TheQuery
}

fragment TheQuery on Query {
  t {
    x
    ... GetU
  }
}

fragment GetU on T {
  u {
    y
    z
  }
}

type Query {
  t: T
}

type T {
  x: Int
  u: U
}

type U {
  y: Int
  z: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
}"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
            assert_eq!(1, rebased_fragments.size());
            assert!(rebased_fragments.contains(&name!("TheQuery")));
            let rebased_fragment = rebased_fragments.fragments.get("TheQuery").unwrap();

            let expected = r#"fragment TheQuery on Query {
  t {
    x
  }
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn handles_subtypes_within_subgraphs() {
        let operation_fragments = r#"
query TestQuery {
  ...TQuery
}

fragment TQuery on Query {
  t {
    x
    y
    ... on T {
      z
    }
  }
}

type Query {
  t: I
}

interface I {
  x: Int
  y: Int
}

type T implements I {
  x: Int
  y: Int
  z: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                NamedFragments::new(&executable_document.fragments, &schema),
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
  y: Int
  z: Int
}
"#;

            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
            assert_eq!(1, rebased_fragments.size());
            assert!(rebased_fragments.contains(&name!("TQuery")));
            let rebased_fragment = rebased_fragments.fragments.get("TQuery").unwrap();

            let expected = r#"fragment TQuery on Query {
  t {
    x
    y
    z
  }
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }
}
