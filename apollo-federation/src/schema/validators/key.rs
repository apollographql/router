use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::KeyDirective;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::DeniesAliases;
use crate::schema::validators::DeniesArguments;
use crate::schema::validators::DeniesDirectiveApplications;
use crate::schema::validators::DenyAliases;
use crate::schema::validators::DenyFieldsWithArguments;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::SchemaFieldSetValidator;
use crate::schema::validators::deny_unsupported_directive_on_interface_type;
use crate::schema::validators::normalize_diagnostic_message;

pub(crate) fn validate_key_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let directive_name = meta
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator<KeyDirective>>> = vec![
        Box::new(DenyUnionAndInterfaceFields::new(schema.schema())),
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplications::new()),
        Box::new(DenyFieldsWithArguments::new()),
    ];

    let allow_on_interface =
        meta.federation_spec_definition().version() >= &Version { major: 2, minor: 3 };

    for key_directive in schema.key_directive_applications()? {
        match key_directive {
            Ok(key) => {
                if !allow_on_interface {
                    deny_unsupported_directive_on_interface_type(
                        &directive_name,
                        &key,
                        schema,
                        errors,
                    );
                }
                match key.parse_fields(schema.schema()) {
                    Ok(fields) => {
                        let existing_error_count = errors.errors.len();
                        for rule in fieldset_rules.iter() {
                            rule.visit(key.target.type_name(), &fields, &key, errors);
                        }

                        // We apply federation-specific validation rules without validating first to maintain compatibility with existing messaging,
                        // but if we get to this point without errors, we want to make sure it's still a valid selection.
                        let did_not_find_errors = existing_error_count == errors.errors.len();
                        if did_not_find_errors
                            && let Err(validation_error) =
                                fields.validate(Valid::assume_valid_ref(schema.schema()))
                        {
                            errors.push(invalid_fields_error_from_diagnostics(
                                &key,
                                validation_error,
                            ));
                        }
                    }
                    Err(e) => errors.push(invalid_fields_error_from_diagnostics(&key, e.errors)),
                }
            }
            Err(e) => errors.push(e),
        }
    }
    Ok(())
}

fn invalid_fields_error_from_diagnostics(
    key: &KeyDirective,
    diagnostics: DiagnosticList,
) -> FederationError {
    let mut errors = MultipleFederationErrors::new();
    for diagnostic in diagnostics.iter() {
        errors.errors.push(SingleFederationError::KeyInvalidFields {
            target_type: key.target.type_name().clone(),
            application: key.schema_directive.to_string(),
            message: normalize_diagnostic_message(diagnostic),
        })
    }
    errors.into()
}

/// Instances of `@key(fields:)` cannot select interface or union fields
struct DenyUnionAndInterfaceFields<'schema> {
    schema: &'schema Schema,
}

impl<'schema> DenyUnionAndInterfaceFields<'schema> {
    fn new(schema: &'schema Schema) -> Self {
        Self { schema }
    }
}

impl SchemaFieldSetValidator<KeyDirective<'_>> for DenyUnionAndInterfaceFields<'_> {
    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        directive: &KeyDirective,
        errors: &mut MultipleFederationErrors,
    ) {
        let inner_ty = field.definition.ty.inner_named_type();
        if let Some(ty) = self.schema.types.get(inner_ty) {
            if ty.is_union() {
                errors
                    .errors
                    .push(SingleFederationError::KeyFieldsSelectInvalidType {
                        target_type: directive.target.type_name().clone(),
                        application: directive.schema_directive.to_string(),
                        message: format!(
                            "field \"{}.{}\" is a Union type which is not allowed in @key",
                            parent_ty, field.name
                        ),
                    })
            } else if ty.is_interface() {
                errors
                    .errors
                    .push(SingleFederationError::KeyFieldsSelectInvalidType {
                        target_type: directive.target.type_name().clone(),
                        application: directive.schema_directive.to_string(),
                        message: format!(
                            "field \"{}.{}\" is an Interface type which is not allowed in @key",
                            parent_ty, field.name
                        ),
                    })
            }
        }
        self.visit_selection_set(
            field.ty().inner_named_type(),
            &field.selection_set,
            directive,
            errors,
        );
    }
}

impl DeniesAliases for KeyDirective<'_> {
    fn error(&self, alias: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::KeyInvalidFields {
            target_type: self.target.type_name().clone(),
            application: self.schema_directive.to_string(),
            message: format!(
                "Cannot use alias \"{alias}\" in \"{alias}: {}\": aliases are not currently supported in @key",
                field.name
            ),
        }
    }
}

impl DeniesArguments for KeyDirective<'_> {
    fn error(&self, type_name: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::KeyFieldsHasArgs {
            target_type: self.target.type_name().clone(),
            application: self.schema_directive.to_string(),
            inner_coordinate: format!("{}.{}", type_name, field.name),
        }
    }
}

impl DeniesDirectiveApplications for KeyDirective<'_> {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError {
        SingleFederationError::KeyHasDirectiveInFieldsArg {
            target_type: self.target.type_name().clone(),
            application: self.schema_directive.to_string(),
            applied_directives: directives.iter().map(|d| d.to_string()).join(", "),
        }
    }
}
