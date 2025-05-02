use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
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
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::DenyNonExternalLeafFields;
use crate::schema::validators::SchemaFieldSetValidator;

pub(crate) fn validate_provides_directives(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let provides_directive_name = metadata
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator<Baggage = ProvidesDirective>>> = vec![
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplicationsInProvides::new()),
        Box::new(DenyFieldsWithArguments::new()),
        Box::new(DenyNonExternalLeafFieldsInProvides::new(
            schema,
            metadata,
            &provides_directive_name,
        )),
    ];

    for provides_directive in schema.provides_directive_applications()? {
        match provides_directive {
            Ok(provides) => {
                // PORT NOTE: In JS, these two checks are done inside the `targetTypeExtractor`.
                if metadata
                    .is_field_external(&FieldDefinitionPosition::Object(provides.target.clone()))
                {
                    errors.errors.push(
                        SingleFederationError::ExternalCollisionWithAnotherDirective {
                            message: format!(
                                "Cannot have both @provides and @external on field \"{}.{}\"",
                                provides.target.type_name, provides.target.field_name
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
                    errors.errors.push(SingleFederationError::ProvidesOnNonObjectField { message: format!("Invalid @provides directive on field \"{}.{}\": field has type \"{}\" which is not a Composite Type", provides.target.type_name, provides.target.field_name, provides.target_return_type) });
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

/// Instances of `@provides(fields:)` cannot use aliases
struct DenyAliases<'a> {
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> DenyAliases<'a> {
    pub(crate) fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyAliases<'a> {
    type Baggage = ProvidesDirective<'a>;

    fn visit_field(
        &self,
        _parent_ty: &Name,
        field: &Field,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        // This largely duplicates the logic of `check_absence_of_aliases`, which was implemented for the QP rewrite.
        // That requires a valid schema and some operation data, which we don't have because were only working with a
        // schema. Additionally, that implementation uses a slightly different error message than that used by the JS
        // version of composition.
        if let Some(alias) = field.alias.as_ref() {
            errors.errors.push(SingleFederationError::ProvidesInvalidFields {
                target_type: baggage.target.type_name.clone(),
                target_field: baggage.target.field_name.clone(),
                application: baggage.schema_directive.to_string(),
                message: format!("Cannot use alias \"{alias}\" in \"{alias}: {}\": aliases are not currently supported in @provides", field.name),
            });
        }
        self.visit_selection_set(
            field.ty().inner_named_type(),
            &field.selection_set,
            baggage,
            errors,
        );
    }
}

/// Instances of `@provides(fields:)` cannot select fields with directive applications
struct DenyFieldsWithDirectiveApplicationsInProvides<'a> {
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> DenyFieldsWithDirectiveApplicationsInProvides<'a> {
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl DenyFieldsWithDirectiveApplications for DenyFieldsWithDirectiveApplicationsInProvides<'_> {
    fn error(&self, directives: &DirectiveList, baggage: &Self::Baggage) -> SingleFederationError {
        SingleFederationError::ProvidesHasDirectiveInFieldsArg {
            target_type: baggage.target.type_name.clone(),
            target_field: baggage.target.field_name.clone(),
            application: baggage.schema_directive.to_string(),
            applied_directives: directives.iter().map(|d| d.to_string()).join(", "),
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyFieldsWithDirectiveApplicationsInProvides<'a> {
    type Baggage = ProvidesDirective<'a>;

    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        DenyFieldsWithDirectiveApplications::visit_field(self, parent_ty, field, baggage, errors);
    }

    fn visit_inline_fragment(
        &self,
        parent_ty: &Name,
        fragment: &apollo_compiler::executable::InlineFragment,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        DenyFieldsWithDirectiveApplications::visit_inline_fragment(
            self, parent_ty, fragment, baggage, errors,
        );
    }
}

/// Instances of `@provides(fields:)` cannot select fields with arguments
struct DenyFieldsWithArguments<'a> {
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> DenyFieldsWithArguments<'a> {
    pub(crate) fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyFieldsWithArguments<'a> {
    type Baggage = ProvidesDirective<'a>;

    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        if !field.definition.arguments.is_empty() {
            errors
                .errors
                .push(SingleFederationError::ProvidesFieldsHasArgs {
                    target_type: baggage.target.type_name.clone(),
                    target_field: baggage.target.field_name.clone(),
                    application: baggage.schema_directive.to_string(),
                    type_name: parent_ty.to_string(),
                    field_name: field.name.to_string(),
                });
        }
        self.visit_selection_set(
            field.ty().inner_named_type(),
            &field.selection_set,
            baggage,
            errors,
        );
    }
}

/// Instances of `@provides(fields:)` must only select external leaf fields
struct DenyNonExternalLeafFieldsInProvides<'a> {
    schema: &'a FederationSchema,
    meta: &'a SubgraphMetadata,
    directive_name: &'a Name,
}

impl<'a> DenyNonExternalLeafFieldsInProvides<'a> {
    fn new(
        schema: &'a FederationSchema,
        meta: &'a SubgraphMetadata,
        directive_name: &'a Name,
    ) -> Self {
        Self {
            schema,
            meta,
            directive_name,
        }
    }
}

impl<'a> DenyNonExternalLeafFields<'a> for DenyNonExternalLeafFieldsInProvides<'a> {
    fn schema(&self) -> &'a FederationSchema {
        self.schema
    }

    fn meta(&self) -> &'a SubgraphMetadata {
        self.meta
    }

    fn directive_name(&self) -> &'a Name {
        self.directive_name
    }

    fn error(&self, message: String, baggage: &Self::Baggage) -> SingleFederationError {
        SingleFederationError::ProvidesFieldsMissingExternal {
            target_type: baggage.target.type_name.clone(),
            target_field: baggage.target.field_name.clone(),
            application: baggage.schema_directive.to_string(),
            message,
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyNonExternalLeafFieldsInProvides<'a> {
    type Baggage = ProvidesDirective<'a>;

    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        DenyNonExternalLeafFields::visit_field(self, parent_ty, field, baggage, errors);
    }
}
