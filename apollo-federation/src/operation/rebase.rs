//! Rebasing takes a selection or a selection set and updates its parent type.
//!
//! Often, the change is between equivalent types from different schemas, but selections can also
//! be rebased from one type to another in the same schema.

use super::Field;
use super::FieldSelection;
use super::InlineFragment;
use super::InlineFragmentSelection;
use super::Selection;
use super::SelectionSet;
use super::TYPENAME_FIELD;
use super::runtime_types_intersect;
use crate::error::FederationError;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::utils::FallibleIterator;

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

impl Selection {
    fn rebase_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Selection, FederationError> {
        match self {
            Selection::Field(field) => field
                .rebase_inner(parent_type, schema)
                .map(|field| field.into()),
            Selection::InlineFragment(inline) => inline.rebase_inner(parent_type, schema),
        }
    }

    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Selection, FederationError> {
        self.rebase_inner(parent_type, schema)
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        match self {
            Selection::Field(field) => field.can_add_to(parent_type, schema),
            Selection::InlineFragment(inline) => inline.can_add_to(parent_type, schema),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum RebaseError {
    #[error(
        "Cannot add selection of field `{field_position}` to selection set of parent type `{parent_type}`"
    )]
    CannotRebase {
        field_position: crate::schema::position::FieldDefinitionPosition,
        parent_type: CompositeTypeDefinitionPosition,
    },
    #[error("Cannot rebase composite field selection because its subselection is empty")]
    EmptySelectionSet,
    #[error(
        "Cannot add fragment of condition `{}` (runtimes: [{}]) to parent type `{}` (runtimes: [{}])",
        type_condition.as_ref().map_or_else(Default::default, |t| t.to_string()),
        type_condition.as_ref().map_or_else(
            || "undefined".to_string(),
            |t| print_possible_runtimes(t, schema),
        ),
        parent_type,
        print_possible_runtimes(parent_type, schema)
    )]
    NonIntersectingCondition {
        type_condition: Option<CompositeTypeDefinitionPosition>,
        parent_type: CompositeTypeDefinitionPosition,
        schema: ValidFederationSchema,
    },
}

impl From<RebaseError> for FederationError {
    fn from(value: RebaseError) -> Self {
        crate::error::SingleFederationError::from(value).into()
    }
}

impl Field {
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Field, FederationError> {
        let field_parent = self.field_position.parent();
        if self.schema == *schema && field_parent == *parent_type {
            // pointing to the same parent -> return self
            return Ok(self.clone());
        }

        if self.name() == &TYPENAME_FIELD {
            let mut updated_field = self.clone();
            updated_field.schema = schema.clone();
            updated_field.field_position = parent_type.introspection_typename_field();
            return Ok(updated_field);
        }

        let field_from_parent = parent_type.field(self.name().clone())?;
        if field_from_parent.try_get(schema.schema()).is_some()
            && self.can_rebase_on(parent_type)?
        {
            let mut updated_field = self.clone();
            updated_field.schema = schema.clone();
            updated_field.field_position = field_from_parent;
            Ok(updated_field)
        } else {
            Err(RebaseError::CannotRebase {
                field_position: self.field_position.clone(),
                parent_type: parent_type.clone(),
            }
            .into())
        }
    }

    /// Verifies whether given field can be rebase on following parent type.
    ///
    /// There are 2 valid cases we want to allow:
    /// 1. either `parent_type` and `field_parent_type` are the same underlying type (same name) but from different underlying schema. Typically,
    ///    happens when we're building subgraph queries but using selections from the original query which is against the supergraph API schema.
    /// 2. or they are not the same underlying type, but the field parent type is from an interface (or an interface object, which is the same
    ///    here), in which case we may be rebasing an interface field on one of the implementation type, which is ok. Note that we don't verify
    ///    that `parent_type` is indeed an implementation of `field_parent_type` because it's possible that this implementation relationship exists
    ///    in the supergraph, but not in any of the subgraph schema involved here. So we just let it be. Not that `rebase_on` will complain anyway
    ///    if the field name simply does not exist in `parent_type`.
    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let field_parent_type = self.field_position.parent();
        // case 1
        if field_parent_type.type_name() == parent_type.type_name() {
            return Ok(true);
        }
        // case 2
        let is_interface_object_type = self
            .schema
            .is_interface_object_type(field_parent_type.clone().into())?;
        Ok(field_parent_type.is_interface_type() || is_interface_object_type)
    }

    fn type_if_added_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Option<OutputTypeDefinitionPosition>, FederationError> {
        let data = self;
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
        if !self.can_rebase_on(parent_type)? {
            return Ok(None);
        }
        let Some(field_definition) = parent_type
            .field(data.field_position.field_name().clone())
            .ok()
            .and_then(|field_pos| field_pos.get(schema.schema()).ok())
        else {
            return Ok(None);
        };
        if let Some(federation_spec_definition) = schema
            .subgraph_metadata()
            .map(|d| d.federation_spec_definition())
        {
            let from_context_directive_definition_name = &federation_spec_definition
                .from_context_directive_definition(schema)?
                .name;
            // We need to ensure that all arguments with `@fromContext` are provided. If the
            // would-be parent type's field has an argument with `@fromContext` and that argument
            // has no value/data in this field, then we return `None` to indicate the rebase isn't
            // possible.
            if field_definition.arguments.iter().any(|arg_definition| {
                arg_definition
                    .directives
                    .has(from_context_directive_definition_name)
                    && !data
                        .arguments
                        .iter()
                        .any(|arg| arg.name == arg_definition.name)
            }) {
                return Ok(None);
            }
        }
        Ok(Some(
            schema
                .get_type(field_definition.ty.inner_named_type().clone())?
                .try_into()?,
        ))
    }
}

impl FieldSelection {
    fn rebase_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<FieldSelection, FederationError> {
        if &self.field.schema == schema && &self.field.field_position.parent() == parent_type {
            // we are rebasing field on the same parent within the same schema - we can just return self
            return Ok(self.clone());
        }

        let rebased = self.field.rebase_on(parent_type, schema)?;
        let Some(selection_set) = &self.selection_set else {
            // leaf field
            return Ok(FieldSelection {
                field: rebased,
                selection_set: None,
            });
        };

        let rebased_type_name = rebased
            .field_position
            .get(schema.schema())?
            .ty
            .inner_named_type();
        let rebased_base_type: CompositeTypeDefinitionPosition =
            schema.get_type(rebased_type_name.clone())?.try_into()?;

        let selection_set_type = &selection_set.type_position;
        if self.field.schema == rebased.schema && &rebased_base_type == selection_set_type {
            // we are rebasing within the same schema and the same base type
            return Ok(FieldSelection {
                field: rebased,
                selection_set: self.selection_set.clone(),
            });
        }

        let rebased_selection_set = selection_set.rebase_inner(&rebased_base_type, schema)?;
        if rebased_selection_set.selections.is_empty() {
            Err(RebaseError::EmptySelectionSet.into())
        } else {
            Ok(FieldSelection {
                field: rebased,
                selection_set: Some(rebased_selection_set),
            })
        }
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        if self.field.schema == *schema && self.field.field_position.parent() == *parent_type {
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

impl InlineFragment {
    fn casted_type_if_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Option<CompositeTypeDefinitionPosition> {
        if self.schema == *schema && self.parent_type_position == *parent_type {
            return Some(self.casted_type());
        }
        let Some(ty) = self.type_condition_position.as_ref() else {
            return Some(parent_type.clone());
        };

        let rebased_type = schema
            .get_type(ty.type_name().clone())
            .ok()
            .and_then(|ty| CompositeTypeDefinitionPosition::try_from(ty).ok())?;

        runtime_types_intersect(parent_type, &rebased_type, schema).then_some(rebased_type)
    }

    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<InlineFragment, FederationError> {
        if self.schema == *schema && self.parent_type_position == *parent_type {
            return Ok(self.clone());
        }

        let type_condition = self.type_condition_position.clone();
        // This usually imply that the fragment is not from the same subgraph than the selection. So we need
        // to update the source type of the fragment, but also "rebase" the condition to the selection set
        // schema.
        let (can_rebase, rebased_condition) = self.can_rebase_on(parent_type, schema);
        if !can_rebase {
            Err(RebaseError::NonIntersectingCondition {
                type_condition,
                parent_type: parent_type.clone(),
                schema: schema.clone(),
            }
            .into())
        } else {
            let mut rebased_fragment = self.clone();
            rebased_fragment.parent_type_position = parent_type.clone();
            rebased_fragment.type_condition_position = rebased_condition;
            rebased_fragment.schema = schema.clone();
            Ok(rebased_fragment)
        }
    }

    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        parent_schema: &ValidFederationSchema,
    ) -> (bool, Option<CompositeTypeDefinitionPosition>) {
        if self.type_condition_position.is_none() {
            // can_rebase = true, condition = undefined
            return (true, None);
        }

        if let Some(Ok(rebased_condition)) = self
            .type_condition_position
            .clone()
            .and_then(|condition_position| {
                parent_schema.try_get_type(condition_position.type_name().clone())
            })
            .map(|rebased_condition_position| {
                CompositeTypeDefinitionPosition::try_from(rebased_condition_position)
            })
        {
            // Root types can always be rebased and the type condition is unnecessary.
            // Moreover, the actual subgraph might have renamed the root types, but the
            // supergraph schema does not contain that information.
            // Note: We only handle when the rebased condition is the same as the parent type. They
            //       could be different in rare cases, but that will be fixed after the
            //       source-awareness initiative is complete.
            if rebased_condition == *parent_type
                && parent_schema.is_root_type(rebased_condition.type_name())
            {
                return (true, None);
            }
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
    fn rebase_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Selection, FederationError> {
        if &self.inline_fragment.schema == schema
            && self.inline_fragment.parent_type_position == *parent_type
        {
            // we are rebasing inline fragment on the same parent within the same schema - we can just return self
            return Ok(self.clone().into());
        }

        let rebased_fragment = self.inline_fragment.rebase_on(parent_type, schema)?;
        let rebased_casted_type = rebased_fragment.casted_type();
        if &self.inline_fragment.schema == schema
            && self.inline_fragment.casted_type() == rebased_casted_type
        {
            // we are within the same schema - selection set does not have to be rebased
            Ok(InlineFragmentSelection::new(rebased_fragment, self.selection_set.clone()).into())
        } else {
            let rebased_selection_set = self
                .selection_set
                .rebase_inner(&rebased_casted_type, schema)?;
            if rebased_selection_set.selections.is_empty() {
                // empty selection set
                Err(RebaseError::EmptySelectionSet.into())
            } else {
                Ok(InlineFragmentSelection::new(rebased_fragment, rebased_selection_set).into())
            }
        }
    }

    fn can_add_to(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        if self.inline_fragment.schema == *schema
            && self.inline_fragment.parent_type_position == *parent_type
        {
            return Ok(true);
        }
        let Some(ty) = self
            .inline_fragment
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
}

impl SelectionSet {
    fn rebase_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<SelectionSet, FederationError> {
        let rebased_results = self
            .selections
            .values()
            .map(|selection| selection.rebase_inner(parent_type, schema));

        Ok(SelectionSet {
            schema: schema.clone(),
            type_position: parent_type.clone(),
            selections: rebased_results
                .collect::<Result<super::SelectionMap, _>>()?
                .into(),
        })
    }

    /// Rebase this selection set so it applies to the given schema and type.
    ///
    /// This can return an empty selection set.
    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<SelectionSet, FederationError> {
        self.rebase_inner(parent_type, schema)
    }

    /// Returns true if the selection set would select cleanly from the given type in the given
    /// schema.
    pub(crate) fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        self.selections
            .values()
            .fallible_all(|selection| selection.can_add_to(parent_type, schema))
    }
}
