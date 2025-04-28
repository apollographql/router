use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::DenyNonExternalLeafFields;
use crate::schema::validators::SchemaFieldSetValidator;

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

/// Instances of `@requires(fields:)` cannot use aliases
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
            errors.errors.push(SingleFederationError::RequiresInvalidFields {
                message: format!("Cannot use alias \"{}\" in \"{}.{}\": aliases are not currently supported in @requires", alias, parent_ty, field.name),
            });
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
    }
}

/// Instances of `@requires(fields:)` cannot select fields with directive applications
struct DenyFieldsWithDirectiveApplicationsInRequires {}

impl DenyFieldsWithDirectiveApplicationsInRequires {
    fn new() -> Self {
        Self {}
    }
}

impl DenyFieldsWithDirectiveApplications for DenyFieldsWithDirectiveApplicationsInRequires {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError {
        SingleFederationError::RequiresHasDirectiveInFieldsArg {
            applied_directives: directives.iter().map(|d| d.name.to_string()).join(", "),
        }
    }
}

impl SchemaFieldSetValidator for DenyFieldsWithDirectiveApplicationsInRequires {
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

    fn error(&self, message: String) -> SingleFederationError {
        SingleFederationError::RequiresFieldsMissingExternal { message }
    }
}

impl SchemaFieldSetValidator for DenyNonExternalLeafFieldsInRequires<'_> {
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
    fn deny_non_external() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @requires(fields: FieldSet!) on FIELD_DEFINITION

            scalar FieldSet

            type Query {
                t: T
            }

            type T {
                f: Int
                g: Int @requires(fields: "f")
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
        let provides = name!("requires");
        let rule = DenyNonExternalLeafFieldsInRequires::new(&fed_schema, &metadata, &provides);
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::RequiresFieldsMissingExternal { .. }
            ),
            "Expected an error about missing @external in @requires(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_directive_applications() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @external on FIELD_DEFINITION | OBJECT

            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            directive @requires(fields: FieldSet!) on FIELD_DEFINITION
            
            scalar FieldSet

            type Query {
                t: T
            }

            type T @key(fields: "id") {
                id: ID
                a: Int @requires(fields: "... @skip(if: false) { b }")
                b: Int @external
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set = FieldSet::parse(
            &schema,
            name!("T"),
            "... @skip(if: false) { b }",
            "test.graphqls",
        )
        .expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyFieldsWithDirectiveApplicationsInRequires::new();
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
                SingleFederationError::RequiresHasDirectiveInFieldsArg { .. }
            ),
            "Expected an error about directive applications in @requires(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_aliases() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @external on FIELD_DEFINITION | OBJECT

            directive @requires(fields: FieldSet!) on FIELD_DEFINITION

            scalar FieldSet

            type Query {
                t: T
            }

            type T {
                x: X @external
                y: Int @external
                g: Int @requires(fields: "foo: y")
                h: Int @requires(fields: "x { m: a n: b }")
            }

            type X {
                a: Int
                b: Int
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set = FieldSet::parse(&schema, name!("T"), "foo: y", "test.graphqls")
            .expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyAliases::new();
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::RequiresInvalidFields { .. }
            ),
            "Expected an error about aliases in @requires(fields:), but got: {:?}",
            errors.errors[0]
        );
    }
}
