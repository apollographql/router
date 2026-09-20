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
        on_non_intersecting: OnNonIntersecting,
    ) -> Result<Selection, FederationError> {
        match self {
            Selection::Field(field) => field
                .rebase_inner(parent_type, schema, on_non_intersecting)
                .map(|field| field.into()),
            Selection::InlineFragment(inline) => {
                inline.rebase_inner(parent_type, schema, on_non_intersecting)
            }
        }
    }

    pub(crate) fn rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Selection, FederationError> {
        self.rebase_inner(parent_type, schema, OnNonIntersecting::Error)
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

/// How rebasing treats a fragment whose type condition cannot intersect the
/// target type: fail the whole rebase, or prune just that branch. Pruning is
/// sound when the caller narrows to a concrete runtime type (type explosion),
/// where a non-intersecting condition can never match at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnNonIntersecting {
    Error,
    Prune,
}

fn prunable_rebase_error(err: &FederationError) -> bool {
    matches!(
        err,
        FederationError::SingleFederationError(
            crate::error::SingleFederationError::InternalRebaseError(
                RebaseError::NonIntersectingCondition { .. } | RebaseError::EmptySelectionSet,
            )
        )
    )
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

        self.rebase_on_inner(parent_type, schema, false)
    }

    /// Like `rebase_on`, but allows rebasing a concrete type's field onto
    /// an interface or @interfaceObject target. Used by the incremental
    /// planner's plan builder where entity fetch paths cross the
    /// concrete-to-interface boundary in @interfaceObject schemas.
    pub(crate) fn rebase_on_for_incremental_planner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<Field, FederationError> {
        let field_parent = self.field_position.parent();
        if self.schema == *schema && field_parent == *parent_type {
            return Ok(self.clone());
        }
        if self.name() == &TYPENAME_FIELD {
            let mut updated_field = self.clone();
            updated_field.schema = schema.clone();
            updated_field.field_position = parent_type.introspection_typename_field();
            return Ok(updated_field);
        }
        self.rebase_on_inner(parent_type, schema, true)
    }

    fn rebase_on_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        allow_interface_target: bool,
    ) -> Result<Field, FederationError> {
        let field_from_parent = parent_type.field(self.name().clone())?;
        if field_from_parent.try_get(schema.schema()).is_some()
            && self.can_rebase_on_inner(parent_type, schema, allow_interface_target)?
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

    /// Verifies whether given field can be rebased on the following parent type.
    ///
    /// There are 2 valid cases:
    /// 1. `parent_type` and `field_parent_type` are the same underlying type
    ///    (same name) but from different schemas. Typical when building
    ///    subgraph queries from supergraph-schema selections.
    /// 2. The field's parent is an interface (or interface object), so we may
    ///    be rebasing an interface field onto an implementing type. We don't
    ///    verify the implementation relationship because it may exist only in
    ///    the supergraph. `rebase_on` will fail if the field doesn't exist.
    fn can_rebase_on(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        target_schema: &ValidFederationSchema,
    ) -> Result<bool, FederationError> {
        self.can_rebase_on_inner(parent_type, target_schema, false)
    }

    /// `allow_interface_target` adds a third case: a concrete type's field on
    /// an interface target, the reverse of case 2. An @interfaceObject source
    /// declares the type as a plain object while the target subgraph has the
    /// interface. `rebase_on` still fails if the interface lacks the field.
    fn can_rebase_on_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        target_schema: &ValidFederationSchema,
        allow_interface_target: bool,
    ) -> Result<bool, FederationError> {
        let field_parent_type = self.field_position.parent();
        // case 1: same type name across schemas
        if field_parent_type.type_name() == parent_type.type_name() {
            return Ok(true);
        }
        // case 2: field parent is an interface or @interfaceObject
        let field_parent_is_iface_obj = self
            .schema
            .is_interface_object_type(field_parent_type.clone().into())?;
        if field_parent_type.is_interface_type() || field_parent_is_iface_obj {
            return Ok(true);
        }
        // case 3: target is an interface or @interfaceObject (incremental
        // planner only, gated by allow_interface_target)
        if allow_interface_target {
            let target_is_iface_obj =
                target_schema.is_interface_object_type(parent_type.clone().into())?;
            return Ok(parent_type.is_interface_type() || target_is_iface_obj);
        }
        Ok(false)
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
            return Ok(Some(data.schema.get_type(base_ty_name)?.try_into()?));
        }
        if data.name() == &TYPENAME_FIELD {
            let Some(type_name) = parent_type
                .introspection_typename_field()
                .try_get(schema.schema())
                .map(|field| field.ty.inner_named_type())
            else {
                return Ok(None);
            };
            return Ok(Some(schema.get_type(type_name)?.try_into()?));
        }
        if !self.can_rebase_on(parent_type, schema)? {
            return Ok(None);
        }
        let Some(field_definition) = parent_type
            .field(data.field_position.field_name().clone())
            .ok()
            .and_then(|field_pos| field_pos.try_get(schema.schema()))
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
                .get_type(field_definition.ty.inner_named_type())?
                .try_into()?,
        ))
    }
}

impl FieldSelection {
    fn rebase_inner(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        on_non_intersecting: OnNonIntersecting,
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
            schema.get_type(rebased_type_name)?.try_into()?;

        let selection_set_type = &selection_set.type_position;
        if self.field.schema == rebased.schema && &rebased_base_type == selection_set_type {
            // we are rebasing within the same schema and the same base type
            return Ok(FieldSelection {
                field: rebased,
                selection_set: self.selection_set.clone(),
            });
        }

        let rebased_selection_set =
            selection_set.rebase_inner(&rebased_base_type, schema, on_non_intersecting)?;
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
            .get_type(ty.type_name())
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
                parent_schema.try_get_type(condition_position.type_name())
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
        on_non_intersecting: OnNonIntersecting,
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
            let rebased_selection_set = self.selection_set.rebase_inner(
                &rebased_casted_type,
                schema,
                on_non_intersecting,
            )?;
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
        on_non_intersecting: OnNonIntersecting,
    ) -> Result<SelectionSet, FederationError> {
        let mut selections = super::SelectionMap::new();
        for selection in self.selections.values() {
            match selection.rebase_inner(parent_type, schema, on_non_intersecting) {
                Ok(rebased) => {
                    selections.insert(rebased);
                }
                Err(err)
                    if on_non_intersecting == OnNonIntersecting::Prune
                        && prunable_rebase_error(&err) => {}
                Err(err) => return Err(err),
            }
        }

        Ok(SelectionSet {
            schema: schema.clone(),
            type_position: parent_type.clone(),
            selections: selections.into(),
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
        self.rebase_inner(parent_type, schema, OnNonIntersecting::Error)
    }

    /// Like [`Self::rebase_on`], but fragments whose type conditions cannot
    /// intersect the target type are pruned instead of failing the rebase.
    /// For use when narrowing to a concrete runtime type, where such branches
    /// can never match.
    pub(crate) fn rebase_on_pruning_non_intersecting(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
    ) -> Result<SelectionSet, FederationError> {
        self.rebase_inner(parent_type, schema, OnNonIntersecting::Prune)
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

#[cfg(test)]
mod tests {
    use super::Field;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::position::FieldDefinitionPosition;
    use crate::schema::position::InterfaceTypeDefinitionPosition;
    use crate::schema::position::ObjectFieldDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::subgraph::test_utils::build_and_validate;

    const INTERFACE_SUBGRAPH: &str = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key"])
        type Query { i: I }
        interface I @key(fields: "id") { id: ID! x: Int }
        type A implements I @key(fields: "id") { id: ID! x: Int }
    "#;

    const INTERFACE_OBJECT_SUBGRAPH: &str = r#"
        extend schema @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@interfaceObject"])
        type Query { is1: [I] }
        type I @interfaceObject @key(fields: "id") { id: ID! x: Int }
    "#;

    fn schema(sdl: &str) -> ValidFederationSchema {
        build_and_validate(sdl).validated_schema().clone()
    }

    /// A concrete `A.x` field, the source side of case 3.
    fn concrete_field(schema: &ValidFederationSchema) -> Field {
        let position = ObjectFieldDefinitionPosition {
            type_name: apollo_compiler::name!("A"),
            field_name: apollo_compiler::name!("x"),
        };
        Field::from_position(schema, FieldDefinitionPosition::Object(position))
    }

    fn assert_cannot_rebase(result: Result<Field, crate::error::FederationError>) {
        let error = result.expect_err("legacy rebase should reject a concrete-to-interface hop");
        assert!(
            error
                .to_string()
                .contains("Cannot add selection of field `A.x`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn case_3_interface_target_rejected_on_legacy_path() {
        let schema = schema(INTERFACE_SUBGRAPH);
        let field = concrete_field(&schema);
        let target = CompositeTypeDefinitionPosition::Interface(InterfaceTypeDefinitionPosition {
            type_name: apollo_compiler::name!("I"),
        });

        assert!(!field.can_rebase_on(&target, &schema).unwrap());
        assert_cannot_rebase(field.rebase_on(&target, &schema));
    }

    #[test]
    fn case_3_interface_target_accepted_on_incremental_planner_path() {
        let schema = schema(INTERFACE_SUBGRAPH);
        let field = concrete_field(&schema);
        let target = CompositeTypeDefinitionPosition::Interface(InterfaceTypeDefinitionPosition {
            type_name: apollo_compiler::name!("I"),
        });

        assert!(field.can_rebase_on_inner(&target, &schema, true).unwrap());
        let rebased = field
            .rebase_on_for_incremental_planner(&target, &schema)
            .expect("incremental planner rebase onto interface");
        assert_eq!(rebased.field_position.parent(), target);
    }

    #[test]
    fn case_3_interface_object_target_rejected_on_legacy_path() {
        let source_schema = schema(INTERFACE_SUBGRAPH);
        let target_schema = schema(INTERFACE_OBJECT_SUBGRAPH);
        let field = concrete_field(&source_schema);
        let target = CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
            type_name: apollo_compiler::name!("I"),
        });

        assert!(!field.can_rebase_on(&target, &target_schema).unwrap());
        assert_cannot_rebase(field.rebase_on(&target, &target_schema));
    }

    #[test]
    fn case_3_interface_object_target_accepted_on_incremental_planner_path() {
        let source_schema = schema(INTERFACE_SUBGRAPH);
        let target_schema = schema(INTERFACE_OBJECT_SUBGRAPH);
        let field = concrete_field(&source_schema);
        let target = CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
            type_name: apollo_compiler::name!("I"),
        });

        assert!(
            field
                .can_rebase_on_inner(&target, &target_schema, true)
                .unwrap()
        );
        let rebased = field
            .rebase_on_for_incremental_planner(&target, &target_schema)
            .expect("incremental planner rebase onto @interfaceObject");
        assert_eq!(rebased.field_position.parent(), target);
        assert_eq!(rebased.schema, target_schema);
    }
}
