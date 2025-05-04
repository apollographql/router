use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
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

    let fieldset_rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
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
                        for rule in fieldset_rules.iter() {
                            rule.visit(key.target.type_name(), &fields, errors);
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

/// Instances of `@key(fields:)` cannot use aliases
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
            errors.errors.push(SingleFederationError::KeyInvalidFields {
                message: format!("Cannot use alias \"{}\" in \"{}.{}\": aliases are not currently supported in @key", alias, parent_ty, field.name),
            });
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
    }
}

/// Instances of `@key(fields:)` cannot select fields with directive applications
struct DenyFieldsWithDirectiveApplicationsInKey {}

impl DenyFieldsWithDirectiveApplicationsInKey {
    fn new() -> Self {
        Self {}
    }
}

impl DenyFieldsWithDirectiveApplications for DenyFieldsWithDirectiveApplicationsInKey {
    fn error(&self, directives: &DirectiveList) -> SingleFederationError {
        SingleFederationError::KeyHasDirectiveInFieldsArg {
            applied_directives: directives.iter().map(|d| d.name.to_string()).join(", "),
        }
    }
}

impl SchemaFieldSetValidator for DenyFieldsWithDirectiveApplicationsInKey {
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

/// Instances of `@key(fields:)` cannot select fields with arguments
struct DenyFieldsWithArguments {}

impl DenyFieldsWithArguments {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl SchemaFieldSetValidator for DenyFieldsWithArguments {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        if !field.definition.arguments.is_empty() {
            errors.errors.push(SingleFederationError::KeyFieldsHasArgs {
                type_name: parent_ty.to_string(),
                field_name: field.name.to_string(),
            });
        }
        self.visit_selection_set(field.ty().inner_named_type(), &field.selection_set, errors);
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::executable::FieldSet;
    use apollo_compiler::name;

    use super::*;

    #[test]
    fn deny_interface_fields() {
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

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::KeyFieldsSelectInvalidType { .. }
            ),
            "Expected an error about interface fields in @key(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_union_fields() {
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

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::KeyFieldsSelectInvalidType { .. }
            ),
            "Expected an error about union fields in @key(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_fields_with_arguments() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            scalar FieldSet

             type Query {
                t: T
            }

            type T @key(fields: "f") {
                f(x: Int): Int
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
                SingleFederationError::KeyFieldsHasArgs { .. }
            ),
            "Expected an error about arguments in @key(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_directive_applications() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            scalar FieldSet

             type Query {
                t: T
            }

            type T @key(fields: "v { x ... @include(if: false) { y }}") {
                v: V
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
            "v { x ... @include(if: false) { y }}",
            "test.graphqls",
        )
        .expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyFieldsWithDirectiveApplicationsInKey::new();
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::KeyHasDirectiveInFieldsArg { .. }
            ),
            "Expected an error about directive applications in @key(fields:), but got: {:?}",
            errors.errors[0]
        );
    }

    #[test]
    fn deny_aliases() {
        let schema = Schema::parse_and_validate(
            r#"
            directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            scalar FieldSet

            type Query {
                t: T
            }

            type T @key(fields: "foo: id") {
                id: ID!
            }
        "#,
            "test.graphqls",
        )
        .expect("parses schema");

        let field_set = FieldSet::parse(&schema, name!("T"), "foo: id", "test.graphqls")
            .expect("parses FieldSet");

        let mut errors = MultipleFederationErrors::new();
        let rule = DenyAliases::new();
        rule.visit(&name!("T"), &field_set, &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0],
                SingleFederationError::KeyInvalidFields { .. }
            ),
            "Expected an error about aliases in @key(fields:), but got: {:?}",
            errors.errors[0]
        );
    }
}
