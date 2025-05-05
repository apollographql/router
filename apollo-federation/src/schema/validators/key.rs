use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
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
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::SchemaFieldSetValidator;
use crate::schema::validators::deny_unsupported_directive_on_interface_type;

pub(crate) fn validate_key_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let directive_name = meta
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator<Baggage = KeyDirective>>> = vec![
        Box::new(DenyUnionAndInterfaceFields::new(schema.schema())),
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplicationsInKey::new()),
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

/// Instances of `@key(fields:)` cannot select interface or union fields
struct DenyUnionAndInterfaceFields<'schema> {
    schema: &'schema Schema,
}

impl<'schema> DenyUnionAndInterfaceFields<'schema> {
    fn new(schema: &'schema Schema) -> Self {
        Self { schema }
    }
}

impl<'schema> SchemaFieldSetValidator for DenyUnionAndInterfaceFields<'schema> {
    type Baggage = KeyDirective<'schema>;

    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        let inner_ty = field.definition.ty.inner_named_type();
        if let Some(ty) = self.schema.types.get(inner_ty) {
            if ty.is_union() {
                errors
                    .errors
                    .push(SingleFederationError::KeyFieldsSelectInvalidType {
                        target_type: baggage.target.type_name().clone(),
                        application: baggage.schema_directive.to_string(),
                        message: format!(
                            "field \"{}.{}\" is a Union type which is not allowed in @key",
                            parent_ty, field.name
                        ),
                    })
            } else if ty.is_interface() {
                errors
                    .errors
                    .push(SingleFederationError::KeyFieldsSelectInvalidType {
                        target_type: baggage.target.type_name().clone(),
                        application: baggage.schema_directive.to_string(),
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
            baggage,
            errors,
        );
    }
}

/// Instances of `@key(fields:)` cannot use aliases
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
    type Baggage = KeyDirective<'a>;

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
            errors.errors.push(SingleFederationError::KeyInvalidFields {
                target_type: baggage.target.type_name().clone(),
                application: baggage.schema_directive.to_string(),
                message: format!("Cannot use alias \"{alias}\" in \"{alias}: {}\": aliases are not currently supported in @key", field.name),
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

/// Instances of `@key(fields:)` cannot select fields with directive applications
struct DenyFieldsWithDirectiveApplicationsInKey<'a> {
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> DenyFieldsWithDirectiveApplicationsInKey<'a> {
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl DenyFieldsWithDirectiveApplications for DenyFieldsWithDirectiveApplicationsInKey<'_> {
    fn error(&self, directives: &DirectiveList, baggage: &Self::Baggage) -> SingleFederationError {
        SingleFederationError::KeyHasDirectiveInFieldsArg {
            target_type: baggage.target.type_name().clone(),
            application: baggage.schema_directive.to_string(),
            applied_directives: directives.iter().map(|d| d.to_string()).join(", "),
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyFieldsWithDirectiveApplicationsInKey<'a> {
    type Baggage = KeyDirective<'a>;

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

/// Instances of `@key(fields:)` cannot select fields with arguments
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
    type Baggage = KeyDirective<'a>;

    fn visit_field(
        &self,
        parent_ty: &Name,
        field: &Field,
        baggage: &Self::Baggage,
        errors: &mut MultipleFederationErrors,
    ) {
        if !field.definition.arguments.is_empty() {
            errors.errors.push(SingleFederationError::KeyFieldsHasArgs {
                target_type: baggage.target.type_name().clone(),
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
