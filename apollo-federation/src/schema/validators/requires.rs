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

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator<Baggage = RequiresDirective>>> = vec![
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplicationsInRequires::new()),
        Box::new(DenyNonExternalLeafFieldsInRequires::new(
            schema,
            meta,
            &requires_directive_name,
        )),
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

/// Instances of `@requires(fields:)` cannot use aliases
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
    type Baggage = RequiresDirective<'a>;

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
            errors.errors.push(SingleFederationError::RequiresInvalidFields {
                target_type: baggage.target.type_name().clone(),
                target_field: baggage.target.field_name().clone(),
                application: baggage.schema_directive.to_string(),
                message: format!("Cannot use alias \"{alias}\" in \"{alias}: {}\": aliases are not currently supported in @requires", field.name),
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

/// Instances of `@requires(fields:)` cannot select fields with directive applications
struct DenyFieldsWithDirectiveApplicationsInRequires<'a> {
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> DenyFieldsWithDirectiveApplicationsInRequires<'a> {
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl DenyFieldsWithDirectiveApplications for DenyFieldsWithDirectiveApplicationsInRequires<'_> {
    fn error(&self, directives: &DirectiveList, baggage: &Self::Baggage) -> SingleFederationError {
        SingleFederationError::RequiresHasDirectiveInFieldsArg {
            target_type: baggage.target.type_name().clone(),
            target_field: baggage.target.field_name().clone(),
            application: baggage.schema_directive.to_string(),
            applied_directives: directives.iter().map(|d| d.to_string()).join(", "),
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyFieldsWithDirectiveApplicationsInRequires<'a> {
    type Baggage = RequiresDirective<'a>;

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

/// Instances of `@requires(fields:)` must only select external leaf fields
struct DenyNonExternalLeafFieldsInRequires<'a> {
    schema: &'a FederationSchema,
    metadata: &'a SubgraphMetadata,
    directive_name: &'a Name,
}

impl<'a> DenyNonExternalLeafFieldsInRequires<'a> {
    fn new(
        schema: &'a FederationSchema,
        metadata: &'a SubgraphMetadata,
        directive_name: &'a Name,
    ) -> Self {
        Self {
            schema,
            metadata,
            directive_name,
        }
    }
}

impl<'a> DenyNonExternalLeafFields<'a> for DenyNonExternalLeafFieldsInRequires<'a> {
    fn schema(&self) -> &'a FederationSchema {
        self.schema
    }

    fn meta(&self) -> &'a SubgraphMetadata {
        self.metadata
    }

    fn directive_name(&self) -> &'a Name {
        self.directive_name
    }

    fn error(&self, message: String, baggage: &Self::Baggage) -> SingleFederationError {
        SingleFederationError::RequiresFieldsMissingExternal {
            target_type: baggage.target_type().clone(),
            target_field: baggage.target.field_name().clone(),
            application: baggage.schema_directive.to_string(),
            message,
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyNonExternalLeafFieldsInRequires<'a> {
    type Baggage = RequiresDirective<'a>;

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
