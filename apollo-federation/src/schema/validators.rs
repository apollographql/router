use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) fn validate_key_directives(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let key_directive_name = metadata
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
        Box::new(DenyUnionAndInterfaceFields::new(schema.schema())),
        Box::new(DenyAliases::new(&key_directive_name)),
        Box::new(DenyDirectiveApplications::new(&key_directive_name)),
        Box::new(DenyFieldsWithArguments::new(&key_directive_name)),
    ];

    for key_directive in schema.key_directive_applications()? {
        match key_directive {
            Ok(key) => match key.parse_fields(schema.schema()) {
                Ok(fields) => {
                    for rule in fieldset_rules.iter() {
                        rule.visit(key.target.type_name(), &fields, errors);
                    }
                }
                Err(e) => errors.push(e.into()),
            },
            Err(e) => errors.push(e),
        }
    }
    Ok(())
}

pub(crate) fn validate_provides_directives(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let provides_directive_name = metadata
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
        Box::new(DenyAliases::new(&provides_directive_name)),
        Box::new(DenyDirectiveApplications::new(&provides_directive_name)),
        Box::new(DenyFieldsWithArguments::new(&provides_directive_name)),
        Box::new(DenyNonExternalLeafFields::new(
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
                    )
                }
                if !schema
                    .get_type(provides.target.type_name.clone())
                    .is_ok_and(|ty| ty.is_composite_type())
                {
                    errors.errors.push(SingleFederationError::ProvidesOnNonObjectField { message: format!("Invalid @provides directive on field \"{}.{}\": field has type \"{}\"", provides.target.type_name, provides.target.field_name, provides.target_return_type) })
                }

                // PORT NOTE: Think of this as `validateFieldSet`, but the set of rules are already filtered to account for what were boolean flags in JS
                match provides.parse_fields(schema.schema()) {
                    Ok(field_set) => {
                        for rule in fieldset_rules.iter() {
                            rule.visit(&provides.target_return_type, &field_set, errors);
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

pub(crate) fn validate_requires_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let requires_directive_name = meta
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC);

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
        Box::new(DenyAliases::new(&requires_directive_name)),
        Box::new(DenyDirectiveApplications::new(&requires_directive_name)),
        Box::new(DenyNonExternalLeafFields::new(
            meta,
            &requires_directive_name,
        )),
    ];

    for requires_directive in schema.requires_directive_applications()? {
        match requires_directive {
            Ok(requires) => match requires.parse_fields(schema.schema()) {
                Ok(fields) => {
                    for rule in fieldset_rules.iter() {
                        rule.visit(&requires.target.type_name, &fields, errors);
                    }
                }
                Err(e) => errors.push(e.into()),
            },
            Err(e) => errors.push(e),
        }
    }
    Ok(())
}

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

struct DenyUnionAndInterfaceFields<'schema> {
    schema: &'schema Schema,
}

impl<'schema> DenyUnionAndInterfaceFields<'schema> {
    fn new(schema: &'schema Schema) -> Self {
        Self { schema }
    }
}

impl SchemaFieldSetValidator for DenyUnionAndInterfaceFields<'_> {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        let inner_ty = field.definition.ty.inner_named_type();
        if let Some(ty) = self.schema.types.get(inner_ty) {
            if ty.is_union() {
                errors
                    .errors
                    .push(SingleFederationError::KeyFieldsSelectInvalidType {
                        message: format!(
                            "field {}.{} is a Union type which is not allowed in @key",
                            parent_ty, field.name
                        ),
                    })
            } else if ty.is_interface() {
                errors
                    .errors
                    .push(SingleFederationError::KeyFieldsSelectInvalidType {
                        message: format!(
                            "field {}.{} is an Interface type which is not allowed in @key",
                            parent_ty, field.name
                        ),
                    })
            }
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
    }
}

struct DenyAliases<'a> {
    directive_name: &'a Name,
}

impl<'a> DenyAliases<'a> {
    fn new(directive_name: &'a Name) -> Self {
        Self { directive_name }
    }
}

impl SchemaFieldSetValidator for DenyAliases<'_> {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        // This largely duuplicates the logic of `check_absence_of_aliases`, which was implemented for the QP rewrite.
        // That requires a valid schema and some operation data, which we don't have because were only working with a
        // schema. Additionally, that implementation uses a slightly different error message than that used by the JS
        // version of composition.
        if let Some(alias) = field.alias.as_ref() {
            errors
                .errors
                .push(SingleFederationError::UnsupportedFeature {
                    message: format!("Cannot use alias \"{}\" in \"{}.{}\": aliases are not currently supported in @{}", alias, parent_ty, field.name, self.directive_name),
                    kind: crate::error::UnsupportedFeatureKind::Alias,
                })
        }
    }
}

struct DenyDirectiveApplications<'a> {
    directive_name: &'a Name,
}

impl<'a> DenyDirectiveApplications<'a> {
    fn new(directive_name: &'a Name) -> Self {
        Self { directive_name }
    }
}

impl<'a> SchemaFieldSetValidator for DenyDirectiveApplications<'a> {
    fn visit_field(&self, _parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        if !field.directives.is_empty() {
            errors
                .errors
                .push(SingleFederationError::UnsupportedFeature {
                    message: format!(
                        "cannot have directive applications in the @{}(fields:) argument but found {}.",
                        self.directive_name,
                        field.directives.iter().map(|d| d.to_string()).join(",")
                    ),
                    kind: crate::error::UnsupportedFeatureKind::Directive,
                })
        }
    }
}

struct DenyFieldsWithArguments<'a> {
    directive_name: &'a Name,
}

impl<'a> DenyFieldsWithArguments<'a> {
    fn new(directive_name: &'a Name) -> Self {
        Self { directive_name }
    }
}

impl<'a> SchemaFieldSetValidator for DenyFieldsWithArguments<'a> {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        if !field.arguments.is_empty() {
            errors
                .errors
                // TODO: Use correct error type for each directive (or consolidate to a single representation)
                .push(SingleFederationError::KeyFieldsHasArgs {
                    message: format!(
                        "field {}.{} cannot be included because it has arguments (fields with argument are not allowed in @{})",
                        parent_ty, field.name, self.directive_name,
                    ),
                })
        }
    }
}

struct DenyNonExternalLeafFields<'a> {
    meta: &'a SubgraphMetadata,
    directive_name: &'a Name,
}

impl<'a> DenyNonExternalLeafFields<'a> {
    fn new(meta: &'a SubgraphMetadata, directive_name: &'a Name) -> Self {
        Self {
            meta,
            directive_name,
        }
    }
}

impl<'a> SchemaFieldSetValidator for DenyNonExternalLeafFields<'a> {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        // TODO: We should probably pass through the directive's target position instead of just name
        let pos = FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
            type_name: parent_ty.clone(),
            field_name: field.name.clone(),
        });
        if self.meta.is_field_external(&pos) {
            // TODO: There's some logic to check implementers if the position is an interface field.
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
                    // TODO: Consolidate error type
                    .push(SingleFederationError::ProvidesFieldsMissingExternal {
                        message: format!("field \"{}.{}\" should not be part of a @{} since it is already \"effectively\" provided by this subgraph (while it is marked @external, it is a @key field of an extension type, which are not internally considered external for historical/backward compatibility reasons)", parent_ty, field.name, self.directive_name),
                    })
            } else {
                errors
                    .errors
                    .push(SingleFederationError::ProvidesFieldsMissingExternal {
                        message: format!("field \"{}.{}\" should not be part of a @{} since it is already provided by this subgraph (it is not marked @external)", parent_ty, field.name, self.directive_name),
                    })
            }
        } else {
            self.visit_selection_set(parent_ty, &field.selection_set, errors);
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;

    #[test]
    fn deny_interface_fields_in_key() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            scalar FieldSet

             type Query {
                t: T
            }

            type T @key(fields: "f") {
                f: I
            }

            interface I {
                i: Int
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set =
            FieldSet::parse(&schema, name!("T"), "f", "test.graphqls").expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyUnionAndInterfaceFields::new(&schema);
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(
            errors.to_string(),
            "The following errors occurred:\n  - field T.f is an Interface type which is not allowed in @key"
        );
    }

    #[test]
    fn deny_union_fields_in_key() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            scalar FieldSet

            type Query {
                t: T
            }

            type T @key(fields: "f") {
                f: U
            }

            union U = Query | T
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set =
            FieldSet::parse(&schema, name!("T"), "f", "test.graphqls").expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyUnionAndInterfaceFields::new(&schema);
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(
            errors.to_string(),
            "The following errors occurred:\n  - field T.f is a Union type which is not allowed in @key"
        );
    }
}
