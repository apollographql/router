use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::ProvidesDirective;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::DeniesAliases;
use crate::schema::validators::DeniesArguments;
use crate::schema::validators::DeniesDirectiveApplications;
use crate::schema::validators::DeniesNonExternalLeafFields;
use crate::schema::validators::DenyAliases;
use crate::schema::validators::DenyFieldsWithArguments;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::DenyNonExternalLeafFields;
use crate::schema::validators::SchemaFieldSetValidator;
use crate::schema::validators::deny_unsupported_directive_on_interface_field;
use crate::schema::validators::normalize_diagnostic_message;

pub(crate) fn validate_provides_directives(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let provides_directive_name = metadata
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator<ProvidesDirective>>> = vec![
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplications::new()),
        Box::new(DenyFieldsWithArguments::new()),
        Box::new(DenyNonExternalLeafFields::new(schema, metadata)),
    ];

    for provides_directive in schema.provides_directive_applications()? {
        match provides_directive {
            Ok(provides) => {
                deny_unsupported_directive_on_interface_field(
                    &provides_directive_name,
                    &provides,
                    schema,
                    errors,
                );

                // PORT NOTE: In JS, these two checks are done inside the `targetTypeExtractor`.
                if metadata.is_field_external(&provides.target.clone().into()) {
                    errors.errors.push(
                        SingleFederationError::ExternalCollisionWithAnotherDirective {
                            message: format!(
                                "Cannot have both @provides and @external on field \"{}.{}\"",
                                provides.target.type_name(),
                                provides.target.field_name()
                            ),
                        },
                    );
                    continue;
                }
                if !schema
                    .schema()
                    .types
                    .get(provides.target_return_type.as_str())
                    .is_some_and(|t| t.is_object() || t.is_interface() || t.is_union())
                {
                    errors.errors.push(SingleFederationError::ProvidesOnNonObjectField { message: format!("Invalid @provides directive on field \"{}.{}\": field has type \"{}\" which is not a Composite Type", provides.target.type_name(), provides.target.field_name(), provides.target_return_type) });
                    continue;
                }

                // PORT NOTE: Think of this as `validateFieldSet`, but the set of rules are already filtered to account for what were boolean flags in JS
                match provides.parse_fields(schema.schema()) {
                    Ok(fields) => {
                        let existing_error_count = errors.errors.len();
                        for rule in fieldset_rules.iter() {
                            rule.visit(provides.target_return_type, &fields, &provides, errors);
                        }

                        // We apply federation-specific validation rules without validating first to maintain compatibility with existing messaging,
                        // but if we get to this point without errors, we want to make sure it's still a valid selection.
                        let did_not_find_errors = existing_error_count == errors.errors.len();
                        if did_not_find_errors
                            && let Err(validation_error) =
                                fields.validate(Valid::assume_valid_ref(schema.schema()))
                        {
                            errors.push(invalid_fields_error_from_diagnostics(
                                &provides,
                                validation_error,
                            ));
                        }
                    }
                    Err(e) => {
                        errors.push(invalid_fields_error_from_diagnostics(&provides, e.errors))
                    }
                }
            }
            Err(e) => errors.push(e),
        }
    }
    Ok(())
}

fn invalid_fields_error_from_diagnostics(
    provides: &ProvidesDirective,
    diagnostics: DiagnosticList,
) -> FederationError {
    let mut errors = MultipleFederationErrors::new();
    for diagnostic in diagnostics.iter() {
        errors
            .errors
            .push(SingleFederationError::ProvidesInvalidFields {
                coordinate: provides.target.coordinate(),
                application: provides.schema_directive.to_string(),
                message: normalize_diagnostic_message(diagnostic),
            })
    }
    errors.into()
}

impl DeniesAliases for ProvidesDirective<'_> {
    fn error(&self, alias: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::ProvidesInvalidFields {
            coordinate: self.target.coordinate(),
            application: self.schema_directive.to_string(),
            message: format!(
                "Cannot use alias \"{alias}\" in \"{alias}: {}\": aliases are not currently supported in @provides",
                field.name
            ),
        }
    }
}

impl DeniesArguments for ProvidesDirective<'_> {
    fn error(&self, parent_ty: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::ProvidesFieldsHasArgs {
            coordinate: self.target.coordinate(),
            application: self.schema_directive.to_string(),
            inner_coordinate: format!("{}.{}", parent_ty, field.name),
        }
    }
}

impl DeniesDirectiveApplications for ProvidesDirective<'_> {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError {
        SingleFederationError::ProvidesHasDirectiveInFieldsArg {
            coordinate: self.target.coordinate(),
            application: self.schema_directive.to_string(),
            applied_directives: directives.iter().map(|d| d.to_string()).join(", "),
        }
    }
}

impl DeniesNonExternalLeafFields for ProvidesDirective<'_> {
    fn error(&self, parent_ty: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::ProvidesFieldsMissingExternal {
            coordinate: self.target.coordinate(),
            application: self.schema_directive.to_string(),
            message: format!(
                "field \"{}.{}\" should not be part of a @provides since it is already provided by this subgraph (it is not marked @external)",
                parent_ty, field.name
            ),
        }
    }

    fn error_for_fake_external_field(
        &self,
        parent_ty: &Name,
        field: &Field,
    ) -> SingleFederationError {
        SingleFederationError::ProvidesFieldsMissingExternal {
            coordinate: self.target.coordinate(),
            application: self.schema_directive.to_string(),
            message: format!(
                "field \"{}.{}\" should not be part of a @provides since it is already \"effectively\" provided by this subgraph (while it is marked @external, it is a @key field of an extension type, which are not internally considered external for historical/backward compatibility reasons)",
                parent_ty, field.name
            ),
        }
    }
}
