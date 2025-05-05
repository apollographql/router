use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::RequiresDirective;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::DeniesAliases;
use crate::schema::validators::DeniesDirectiveApplications;
use crate::schema::validators::DeniesNonExternalLeafFields;
use crate::schema::validators::DenyAliases;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::DenyNonExternalLeafFields;
use crate::schema::validators::SchemaFieldSetValidator;
use crate::schema::validators::deny_unsupported_directive_on_interface_field;

pub(crate) fn validate_requires_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let requires_directive_name = meta
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator<RequiresDirective>>> = vec![
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplications::new()),
        Box::new(DenyNonExternalLeafFields::new(schema, meta)),
    ];

    for requires_directive in schema.requires_directive_applications()? {
        match requires_directive {
            Ok(requires) => {
                deny_unsupported_directive_on_interface_field(
                    &requires_directive_name,
                    &requires,
                    schema,
                    errors,
                );
                match requires.parse_fields(schema.schema()) {
                    Ok(fields) => {
                        let existing_error_count = errors.errors.len();
                        for rule in fieldset_rules.iter() {
                            rule.visit(&requires.target.type_name(), &fields, &requires, errors);
                        }

                        // We apply federation-specific validation rules without validating first to maintain compatibility with existing messaging,
                        // but if we get to this point without errors, we want to make sure it's still a valid selection.
                        let did_not_find_errors = existing_error_count == errors.errors.len();
                        if did_not_find_errors {
                            if let Err(validation_error) =
                                fields.validate(Valid::assume_valid_ref(schema.schema()))
                            {
                                errors.push(validation_error.into());
                            }
                        }
                    }
                    Err(e) => errors.push(e.into()),
                }
            }
            Err(e) => errors.push(e),
        }
    }
    Ok(())
}

impl DeniesAliases for RequiresDirective<'_> {
    fn error(&self, alias: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::RequiresInvalidFields {
            target_type: self.target.type_name().clone(),
            target_field: self.target.field_name().clone(),
            application: self.schema_directive.to_string(),
            message: format!(
                "Cannot use alias \"{alias}\" in \"{alias}: {}\": aliases are not currently supported in @requires",
                field.name
            ),
        }
    }
}

impl DeniesDirectiveApplications for RequiresDirective<'_> {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError {
        SingleFederationError::RequiresHasDirectiveInFieldsArg {
            target_type: self.target.type_name().clone(),
            target_field: self.target.field_name().clone(),
            application: self.schema_directive.to_string(),
            applied_directives: directives.iter().map(|d| d.to_string()).join(", "),
        }
    }
}

impl DeniesNonExternalLeafFields for RequiresDirective<'_> {
    fn error(&self, parent_ty: &Name, field: &Field) -> SingleFederationError {
        SingleFederationError::RequiresFieldsMissingExternal {
            target_type: self.target.type_name().clone(),
            target_field: self.target.field_name().clone(),
            application: self.schema_directive.to_string(),
            message: format!(
                "field \"{}.{}\" should not be part of a @requires since it is already provided by this subgraph (it is not marked @external)",
                parent_ty, field.name
            ),
        }
    }

    fn error_for_fake_external_field(
        &self,
        parent_ty: &Name,
        field: &Field,
    ) -> SingleFederationError {
        SingleFederationError::RequiresFieldsMissingExternal {
            target_type: self.target.type_name().clone(),
            target_field: self.target.field_name().clone(),
            application: self.schema_directive.to_string(),
            message: format!(
                "field \"{}.{}\" should not be part of a @requires since it is already \"effectively\" provided by this subgraph (while it is marked @external, it is a @key field of an extension type, which are not internally considered external for historical/backward compatibility reasons)",
                parent_ty, field.name
            ),
        }
    }
}
