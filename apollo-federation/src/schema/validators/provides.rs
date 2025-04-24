use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
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

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
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
                            rule.visit(provides.target_return_type, &field_set, errors);
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
struct DenyAliases {}

impl DenyAliases {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl SchemaFieldSetValidator for DenyAliases {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        // This largely duuplicates the logic of `check_absence_of_aliases`, which was implemented for the QP rewrite.
        // That requires a valid schema and some operation data, which we don't have because were only working with a
        // schema. Additionally, that implementation uses a slightly different error message than that used by the JS
        // version of composition.
        if let Some(alias) = field.alias.as_ref() {
            errors.errors.push(SingleFederationError::ProvidesInvalidFields {
                message: format!("Cannot use alias \"{}\" in \"{}.{}\": aliases are not currently supported in @provides", alias, parent_ty, field.name),
            });
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
    }
}

/// Instances of `@provides(fields:)` cannot select fields with directive applications
struct DenyFieldsWithDirectiveApplicationsInProvides {}

impl DenyFieldsWithDirectiveApplicationsInProvides {
    fn new() -> Self {
        Self {}
    }
}

impl DenyFieldsWithDirectiveApplications for DenyFieldsWithDirectiveApplicationsInProvides {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError {
        SingleFederationError::ProvidesHasDirectiveInFieldsArg {
            applied_directives: directives.iter().map(|d| d.name.to_string()).join(", "),
        }
    }
}

impl SchemaFieldSetValidator for DenyFieldsWithDirectiveApplicationsInProvides {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        DenyFieldsWithDirectiveApplications::visit_field(self, parent_ty, field, errors);
    }

    fn visit_inline_fragment(
        &self,
        parent_ty: &Name,
        fragment: &apollo_compiler::executable::InlineFragment,
        errors: &mut MultipleFederationErrors,
    ) {
        DenyFieldsWithDirectiveApplications::visit_inline_fragment(
            self, parent_ty, fragment, errors,
        );
    }
}

/// Instances of `@provides(fields:)` cannot select fields with arguments
struct DenyFieldsWithArguments {}

impl DenyFieldsWithArguments {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl SchemaFieldSetValidator for DenyFieldsWithArguments {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        if !field.definition.arguments.is_empty() {
            errors
                .errors
                .push(SingleFederationError::ProvidesFieldsHasArgs {
                    type_name: parent_ty.to_string(),
                    field_name: field.name.to_string(),
                });
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
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

    fn error(&self, message: String) -> SingleFederationError {
        SingleFederationError::ProvidesFieldsMissingExternal { message }
    }
}

impl SchemaFieldSetValidator for DenyNonExternalLeafFieldsInProvides<'_> {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        DenyNonExternalLeafFields::visit_field(self, parent_ty, field, errors);
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::name;

    use super::*;
    use crate::schema::compute_subgraph_metadata;

    #[test]
    fn deny_fields_with_arguments() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @external on FIELD_DEFINITION | OBJECT

            directive @provides(fields: FieldSet!) on FIELD_DEFINITION

            scalar FieldSet

            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f(x: Int): Int @external
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set =
            FieldSet::parse(&schema, name!("T"), "f", "test.graphqls").expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyFieldsWithArguments::new();
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::ProvidesFieldsHasArgs { .. }
            ),
            "Expected an error about arguments in @provides(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_non_external() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @provides(fields: FieldSet!) on FIELD_DEFINITION

            scalar FieldSet

            type Query {
                t: T @provides(fields: "f")
            }

            type T {
                f: Int
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");
        let fed_schema = FederationSchema::new((*schema).clone()).expect("wraps schema");
        let metadata = compute_subgraph_metadata(&fed_schema)
            .expect("computes metadata")
            .expect("has metadata");

        let field_set =
            FieldSet::parse(&schema, name!("T"), "f", "test.graphqls").expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let provides = name!("provides");
        let rule = DenyNonExternalLeafFieldsInProvides::new(&fed_schema, &metadata, &provides);
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::ProvidesFieldsMissingExternal { .. }
            ),
            "Expected an error about missing @external in @provides(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_directive_applications() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @external on FIELD_DEFINITION | OBJECT

            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            directive @provides(fields: FieldSet!) on FIELD_DEFINITION
            
            scalar FieldSet

            type Query {
                t: T @provides(fields: "v { ... on V @skip(if: true) { x y } }")
            }

            type T @key(fields: "id") {
                id: ID
                v: V @external
            }

            type V {
                x: Int
                y: Int
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set = FieldSet::parse(
            &schema,
            name!("T"),
            "v { ... on V @skip(if: true) { x y } }",
            "test.graphqls",
        )
        .expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyFieldsWithDirectiveApplicationsInProvides::new();
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(
            errors.errors.len(),
            1,
            "Expected one error, got {:?}",
            errors.errors
        );
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::ProvidesHasDirectiveInFieldsArg { .. }
            ),
            "Expected an error about directive applications in @provides(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_aliases() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @external on FIELD_DEFINITION | OBJECT

            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            directive @provides(fields: FieldSet!) on FIELD_DEFINITION

            scalar FieldSet

            type Query {
                t: T @provides(fields: "bar: x")
            }

            type T @key(fields: "id") {
                id: ID!
                x: Int @external
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set = FieldSet::parse(&schema, name!("T"), "bar: x", "test.graphqls")
            .expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyAliases::new();
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::ProvidesInvalidFields { .. }
            ),
            "Expected an error about aliases in @provides(fields:), but got: {:?}",
            errors.errors[0]
        );
    }
}
