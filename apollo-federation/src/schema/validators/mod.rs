use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::KeyDirective;
use crate::schema::ProvidesDirective;
use crate::schema::RequiresDirective;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) mod external;
pub(crate) mod key;
pub(crate) mod provides;
pub(crate) mod requires;

/// A trait for validating FieldSets used in schema directives. Do not use this
/// to validate FieldSets used in operations. This will skip named fragments
/// because they aren't available in the context of a schema.
trait SchemaFieldSetValidator {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors);

    fn visit(&self, parent_ty: &Name, field_set: &FieldSet, errors: &mut MultipleFederationErrors) {
        self.visit_selection_set(parent_ty, &field_set.selection_set, errors)
    }

    fn visit_inline_fragment(
        &self,
        parent_ty: &Name,
        fragment: &InlineFragment,
        errors: &mut MultipleFederationErrors,
    ) {
        self.visit_selection_set(
            fragment.type_condition.as_ref().unwrap_or(parent_ty),
            &fragment.selection_set,
            errors,
        );
    }

    fn visit_selection_set(
        &self,
        parent_ty: &Name,
        selection_set: &SelectionSet,
        errors: &mut MultipleFederationErrors,
    ) {
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.visit_field(parent_ty, field, errors);
                }
                Selection::FragmentSpread(_) => {
                    // no-op; fragment spreads are not supported in schemas
                }
                Selection::InlineFragment(fragment) => {
                    self.visit_inline_fragment(parent_ty, fragment, errors);
                }
            }
        }
    }
}

trait DenyFieldsWithDirectiveApplications: SchemaFieldSetValidator {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError;

    fn visit_field(&self, _parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        if !field.directives.is_empty() {
            errors.errors.push(self.error(&field.directives))
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
    }

    fn visit_inline_fragment(
        &self,
        parent_ty: &Name,
        fragment: &InlineFragment,
        errors: &mut MultipleFederationErrors,
    ) {
        if !fragment.directives.is_empty() {
            errors.errors.push(self.error(&fragment.directives));
        }
        self.visit_selection_set(
            fragment.type_condition.as_ref().unwrap_or(parent_ty),
            &fragment.selection_set,
            errors,
        );
    }
}

trait DenyNonExternalLeafFields<'a>: SchemaFieldSetValidator {
    fn schema(&self) -> &'a FederationSchema;
    fn meta(&self) -> &'a SubgraphMetadata;
    fn directive_name(&self) -> &'a Name;
    fn error(&self, message: String) -> SingleFederationError;

    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        let pos = if self.schema().is_interface(parent_ty) {
            FieldDefinitionPosition::Interface(InterfaceFieldDefinitionPosition {
                type_name: parent_ty.clone(),
                field_name: field.name.clone(),
            })
        } else {
            FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
                type_name: parent_ty.clone(),
                field_name: field.name.clone(),
            })
        };

        if self.meta().is_field_external(&pos)
            || (pos.is_interface() && self.meta().is_field_external_in_implementer(&pos))
        {
            return;
        }

        // PORT_NOTE: In JS, this uses a `hasExternalInParents` flag to determine if the field is external.
        // Since this logic is isolated to this one rule, we return early if we encounter an external field,
        // so we know that no parent is external if we make it to this point.
        let is_leaf = field.selection_set.is_empty();
        if is_leaf {
            if self.meta().is_field_fake_external(&pos) {
                errors
                    .errors
                    .push(self.error(format!("field \"{}.{}\" should not be part of a @{} since it is already \"effectively\" provided by this subgraph (while it is marked @external, it is a @key field of an extension type, which are not internally considered external for historical/backward compatibility reasons)", parent_ty, field.name, self.directive_name())));
            } else {
                errors
                    .errors
                    .push(self.error(format!("field \"{}.{}\" should not be part of a @{} since it is already provided by this subgraph (it is not marked @external)", parent_ty, field.name, self.directive_name()),
                    ));
            }
        } else {
            self.visit_selection_set(parent_ty, &field.selection_set, errors);
        }
    }
}

pub(crate) trait AppliesOnType {
    fn unsupported_on_interface_error(message: String) -> SingleFederationError;
}

impl AppliesOnType for KeyDirective<'_> {
    fn unsupported_on_interface_error(message: String) -> SingleFederationError {
        SingleFederationError::KeyUnsupportedOnInterface { message }
    }
}

pub(crate) trait AppliesOnField {
    fn applied_field(&self) -> &ObjectOrInterfaceFieldDefinitionPosition;
    fn unsupported_on_interface_error(message: String) -> SingleFederationError;
}

impl AppliesOnField for RequiresDirective<'_> {
    fn applied_field(&self) -> &ObjectOrInterfaceFieldDefinitionPosition {
        &self.target
    }

    fn unsupported_on_interface_error(message: String) -> SingleFederationError {
        SingleFederationError::RequiresUnsupportedOnInterface { message }
    }
}

impl AppliesOnField for ProvidesDirective<'_> {
    fn applied_field(&self) -> &ObjectOrInterfaceFieldDefinitionPosition {
        &self.target
    }

    fn unsupported_on_interface_error(message: String) -> SingleFederationError {
        SingleFederationError::ProvidesUnsupportedOnInterface { message }
    }
}

pub(crate) fn deny_unsupported_directive_on_interface_type<D: HasFields + AppliesOnType>(
    directive_name: &Name,
    directive_application: &D,
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) {
    let directive_display = format!("@{directive_name}");
    let target_type = directive_application.target_type();
    if schema.is_interface(target_type) {
        errors.push(
            D::unsupported_on_interface_error(
                format!(
                    r#"Cannot use {directive_display} on interface "{target_type}": {directive_display} is not yet supported on interfaces"#,
                ),
            )
            .into(),
        );
    }
}

pub(crate) fn deny_unsupported_directive_on_interface_field<D: HasFields + AppliesOnField>(
    directive_name: &Name,
    directive_application: &D,
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) {
    let directive_display = format!("@{directive_name}");
    let applied_field = directive_application.applied_field();
    let parent_type = applied_field.parent();
    if schema.is_interface(parent_type.type_name()) {
        errors.push(
            D::unsupported_on_interface_error(
                format!(
                    r#"Cannot use {directive_display} on field "{applied_field}" of parent type "{parent_type}": {directive_display} is not yet supported within interfaces"#,
                ),
            )
            .into(),
        );
    }
}
