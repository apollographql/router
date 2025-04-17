use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
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
    let rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
        Box::new(DenyUnionAndInterfaceFields::new(schema.schema())),
        Box::new(DenyAliases::new(key_directive_name.clone())),
        Box::new(DenyDirectiveApplications::new(key_directive_name.clone())),
        Box::new(DenyFieldsWithArguments::new(key_directive_name.clone())),
    ];
    for key_directive in schema.key_directive_applications()? {
        match key_directive {
            Ok(key) => {
                match FieldSet::parse_and_validate(
                    Valid::assume_valid_ref(schema.schema()),
                    key.target.type_name().clone(),
                    key.arguments.fields,
                    "field_set.graphql",
                ) {
                    Ok(field_set) => {
                        for rule in rules.iter() {
                            rule.visit(key.target.type_name(), &field_set, errors);
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

pub(crate) fn validate_provides_directives(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let provides_directive_name = metadata
        .federation_spec_definition()
        .directive_name_in_schema(schema, &FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC)?
        .unwrap_or(FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC);
    let rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
        Box::new(DenyAliases::new(provides_directive_name.clone())),
        Box::new(DenyDirectiveApplications::new(
            provides_directive_name.clone(),
        )),
        Box::new(DenyFieldsWithArguments::new(
            provides_directive_name.clone(),
        )),
        Box::new(DenyNonExternalLeafFields::new(
            metadata.clone(), // TODO: Definitely don't clone this
            provides_directive_name.clone(),
        )),
    ];
    for provides_directive in schema.provides_directive_applications()? {
        match provides_directive {
            Ok(provides) => {
                match FieldSet::parse_and_validate(
                    Valid::assume_valid_ref(schema.schema()),
                    provides.target_return_type.clone(),
                    provides.arguments.fields,
                    "field_set.graphql",
                ) {
                    Ok(field_set) => {
                        for rule in rules.iter() {
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
    let rules: Vec<Box<dyn SchemaFieldSetValidator>> = vec![
        Box::new(DenyAliases::new(requires_directive_name.clone())),
        Box::new(DenyDirectiveApplications::new(
            requires_directive_name.clone(),
        )),
        Box::new(DenyNonExternalLeafFields::new(
            meta.clone(), // TODO: Definitely don't clone this
            requires_directive_name.clone(),
        )),
    ];
    for requires_directive in schema.requires_directive_applications()? {
        match requires_directive {
            Ok(requires) => {
                match FieldSet::parse_and_validate(
                    Valid::assume_valid_ref(schema.schema()),
                    requires.target.type_name.clone(),
                    requires.arguments.fields,
                    "field_set.graphql",
                ) {
                    Ok(field_set) => {
                        for rule in rules.iter() {
                            rule.visit(&requires.target.type_name, &field_set, errors);
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

struct DenyAliases {
    directive_name: Name,
}

impl DenyAliases {
    fn new(directive_name: Name) -> Self {
        Self { directive_name }
    }
}

impl SchemaFieldSetValidator for DenyAliases {
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

struct DenyDirectiveApplications {
    directive_name: Name,
}

impl DenyDirectiveApplications {
    fn new(directive_name: Name) -> Self {
        Self { directive_name }
    }
}

impl SchemaFieldSetValidator for DenyDirectiveApplications {
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

struct DenyFieldsWithArguments {
    directive_name: Name,
}

impl DenyFieldsWithArguments {
    fn new(directive_name: Name) -> Self {
        Self { directive_name }
    }
}
impl SchemaFieldSetValidator for DenyFieldsWithArguments {
    fn visit_field(&self, parent_ty: &Name, field: &Field, errors: &mut MultipleFederationErrors) {
        if !field.arguments.is_empty() {
            errors
                .errors
                // TODO: Use correct error type for each directive (or consolidate to a single representation)
                .push(SingleFederationError::KeyFieldsHasArgs {
                    message: format!(
                        "field {}.{} cannot be included because it has arguments (fields with argument are not allowed in @{})",
                        parent_ty, field.name,
                        self.directive_name,
                    ),
                })
        }
    }
}

struct DenyNonExternalLeafFields {
    meta: SubgraphMetadata,
    directive_name: Name,
}

impl DenyNonExternalLeafFields {
    fn new(meta: SubgraphMetadata, directive_name: Name) -> Self {
        Self {
            meta,
            directive_name,
        }
    }
}

impl SchemaFieldSetValidator for DenyNonExternalLeafFields {
    /**
    * const mustBeExternal = !selection.selectionSet && !allowOnNonExternalLeafFields && !hasExternalInParents;
     if (!isExternal && mustBeExternal) {
       const errorCode = ERROR_CATEGORIES.DIRECTIVE_FIELDS_MISSING_EXTERNAL.get(directiveName);
       if (metadata.isFieldFakeExternal(field)) {
         onError(errorCode.err(
           `field "${field.coordinate}" should not be part of a @${directiveName} since it is already "effectively" provided by this subgraph `
             + `(while it is marked @${FederationDirectiveName.EXTERNAL}, it is a @${FederationDirectiveName.KEY} field of an extension type, which are not internally considered external for historical/backward compatibility reasons)`,
           { nodes: field.sourceAST }
         ));
       } else {
         onError(errorCode.err(
           `field "${field.coordinate}" should not be part of a @${directiveName} since it is already provided by this subgraph (it is not marked @${FederationDirectiveName.EXTERNAL})`,
           { nodes: field.sourceAST }
         ));
       }
     }
    */
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

pub(crate) struct DenyExternalParent {
    meta: SubgraphMetadata,
}

impl DenyExternalParent {
    pub(crate) fn new(meta: SubgraphMetadata) -> Self {
        Self { meta }
    }
}

impl SchemaFieldSetValidator for DenyExternalParent {
    fn visit(&self, parent_ty: &Name, field_set: &FieldSet, errors: &mut MultipleFederationErrors) {
        // TODO: if self.meta.is_field_external(field) {}
    }

    fn visit_field(
        &self,
        _parent_ty: &Name,
        _field: &Field,
        _errors: &mut MultipleFederationErrors,
    ) {
        // no-op; we only care about the top-level call to `visit`
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
