use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::diagnostic::Diagnostic;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::DiagnosticData;

use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::KeyDirective;
use crate::schema::ProvidesDirective;
use crate::schema::RequiresDirective;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) mod cache_invalidation;
pub(crate) mod cache_tag;
pub(crate) mod context;
pub(crate) mod cost;
pub(crate) mod external;
pub(crate) mod from_context;
pub(crate) mod interface_object;
pub(crate) mod key;
pub(crate) mod list_size;
pub(crate) mod merged;
pub(crate) mod provides;
pub(crate) mod requires;
pub(crate) mod root_fields;
pub(crate) mod shareable;
pub(crate) mod tag;

/// A trait for validating FieldSets used in schema directives. Do not use this
/// to validate FieldSets used in operations. This will skip named fragments
/// because they aren't available in the context of a schema.
trait SchemaFieldSetValidator<D> {
    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    );

    fn visit(
        &self,
        parent_ty: &Name,
        field_set: &FieldSet,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        self.visit_selection_set(parent_ty, &field_set.selection_set, directive, errors)
    }

    fn visit_inline_fragment(
        &self,
        parent_ty: &Name,
        fragment: &InlineFragment,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        self.visit_selection_set(
            fragment.type_condition.as_ref().unwrap_or(parent_ty),
            &fragment.selection_set,
            directive,
            errors,
        );
    }

    fn visit_selection_set(
        &self,
        parent_ty: &Name,
        selection_set: &SelectionSet,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.visit_field(parent_ty, field, directive, errors);
                }
                Selection::FragmentSpread(_) => {
                    // no-op; fragment spreads are not supported in schemas
                }
                Selection::InlineFragment(fragment) => {
                    self.visit_inline_fragment(parent_ty, fragment, directive, errors);
                }
            }
        }
    }
}

trait DeniesAliases {
    fn error(&self, alias: &Name, field: &Field) -> SingleFederationError;
}

pub(crate) struct DenyAliases {}

impl DenyAliases {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl<D: DeniesAliases> SchemaFieldSetValidator<D> for DenyAliases {
    fn visit_field(
        &self,
        _parent_ty: &Name,
        field: &Field,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        // This largely duplicates the logic of `check_absence_of_aliases`, which was implemented for the QP rewrite.
        // That requires a valid schema and some operation data, which we don't have because were only working with a
        // schema. Additionally, that implementation uses a slightly different error message than that used by the JS
        // version of composition.
        if let Some(alias) = field.alias.as_ref() {
            errors.errors.push(directive.error(alias, field));
        }
        self.visit_selection_set(
            field.ty().inner_named_type(),
            &field.selection_set,
            directive,
            errors,
        );
    }
}

trait DeniesArguments {
    fn error(&self, parent_ty: &Name, field: &Field) -> SingleFederationError;
}

pub(crate) struct DenyFieldsWithArguments {}

impl DenyFieldsWithArguments {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl<D: DeniesArguments> SchemaFieldSetValidator<D> for DenyFieldsWithArguments {
    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        if !field.definition.arguments.is_empty() {
            errors.errors.push(directive.error(parent_ty, field));
        }
        self.visit_selection_set(
            field.ty().inner_named_type(),
            &field.selection_set,
            directive,
            errors,
        );
    }
}

trait DeniesDirectiveApplications {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError;
}

pub(crate) struct DenyFieldsWithDirectiveApplications {}

impl DenyFieldsWithDirectiveApplications {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl<D: DeniesDirectiveApplications> SchemaFieldSetValidator<D>
    for DenyFieldsWithDirectiveApplications
{
    fn visit_field(
        &self,
        _parent_ty: &Name,
        field: &Field,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        if !field.directives.is_empty() {
            errors.errors.push(directive.error(&field.directives))
        }
        self.visit_selection_set(
            field.ty().inner_named_type(),
            &field.selection_set,
            directive,
            errors,
        );
    }

    fn visit_inline_fragment(
        &self,
        parent_ty: &Name,
        fragment: &InlineFragment,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        if !fragment.directives.is_empty() {
            errors.errors.push(directive.error(&fragment.directives));
        }
        self.visit_selection_set(
            fragment.type_condition.as_ref().unwrap_or(parent_ty),
            &fragment.selection_set,
            directive,
            errors,
        );
    }
}

trait DeniesNonExternalLeafFields {
    fn error(&self, parent_ty: &Name, field: &Field) -> SingleFederationError;
    fn error_for_fake_external_field(
        &self,
        parent_ty: &Name,
        field: &Field,
    ) -> SingleFederationError;
}

struct DenyNonExternalLeafFields<'schema> {
    schema: &'schema FederationSchema,
    meta: &'schema SubgraphMetadata,
}

impl<'schema> DenyNonExternalLeafFields<'schema> {
    pub(crate) fn new(schema: &'schema FederationSchema, meta: &'schema SubgraphMetadata) -> Self {
        Self { schema, meta }
    }
}

impl<D: DeniesNonExternalLeafFields> SchemaFieldSetValidator<D> for DenyNonExternalLeafFields<'_> {
    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        directive: &D,
        errors: &mut MultipleFederationErrors,
    ) {
        let pos = if self.schema.is_interface(parent_ty) {
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

        if self.meta.is_field_external(&pos)
            || (pos.is_interface() && self.meta.is_field_external_in_implementer(&pos))
        {
            return;
        }

        // PORT_NOTE: In JS, this uses a `hasExternalInParents` flag to determine if the field is external.
        // Since this logic is isolated to this one rule, we return early if we encounter an external field,
        // so we know that no parent is external if we make it to this point.
        let is_leaf = field.selection_set.is_empty();
        if is_leaf {
            if self.meta.is_field_fake_external(&pos) {
                errors
                    .errors
                    .push(directive.error_for_fake_external_field(parent_ty, field));
            } else {
                errors.errors.push(directive.error(parent_ty, field));
            }
        } else {
            self.visit_selection_set(
                field.ty().inner_named_type(),
                &field.selection_set,
                directive,
                errors,
            );
        }
    }
}

pub(crate) trait AppliesOnType {
    fn applied_type(&self) -> &Name;
    fn unsupported_on_interface_error(message: String) -> SingleFederationError;
}

impl AppliesOnType for KeyDirective<'_> {
    fn applied_type(&self) -> &Name {
        self.target.type_name()
    }

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

pub(crate) fn deny_unsupported_directive_on_interface_type<D: AppliesOnType>(
    directive_name: &Name,
    directive_application: &D,
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) {
    let applied_type = directive_application.applied_type();
    if schema.is_interface(applied_type) {
        let directive_display = format!("@{directive_name}");
        errors.push(
            D::unsupported_on_interface_error(
                format!(
                    r#"Cannot use {directive_display} on interface "{applied_type}": {directive_display} is not yet supported on interfaces"#,
                ),
            )
            .into(),
        );
    }
}

pub(crate) fn deny_unsupported_directive_on_interface_field<D: AppliesOnField>(
    directive_name: &Name,
    directive_application: &D,
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) {
    let applied_field = directive_application.applied_field();
    let parent_type = applied_field.parent();
    if schema.is_interface(parent_type.type_name()) {
        let directive_display = format!("@{directive_name}");
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

pub(crate) fn normalize_diagnostic_message(diagnostic: Diagnostic<'_, DiagnosticData>) -> String {
    diagnostic
        .error
        .unstable_compat_message() // Attempt to convert to something closer to the original JS error messages
        .unwrap_or_else(|| diagnostic.error.to_string()) // Using `diagnostic.error` strips the potentially misleading location info from the message
        .replace("syntax error:", "Syntax error:")
}
