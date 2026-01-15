//! The `@mapping` directive for reusable selection fragments.
//!
//! This directive can be applied to GraphQL types to define reusable JSON-to-GraphQL
//! field mappings that can be referenced in `@connect` selection strings using
//! `...TypeName` spread syntax.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::name;

use crate::connectors::ConnectSpec;
use crate::connectors::spec::connect_spec_from_schema;
use crate::error::FederationError;

/// The name of the `@mapping` directive in the spec
pub(crate) const MAPPING_DIRECTIVE_NAME_IN_SPEC: Name = name!("mapping");

/// The `as` argument for aliasing the mapping name
pub(super) const MAPPING_AS_ARGUMENT_NAME: Name = name!("as");

/// Default connect spec to use when none is found
pub(super) const DEFAULT_CONNECT_SPEC: ConnectSpec = ConnectSpec::V0_5;

/// Arguments extracted from a `@mapping` directive
#[derive(Debug, Clone)]
pub(crate) struct MappingDirectiveArguments {
    /// The GraphQL type this mapping is defined on
    pub type_name: Name,

    /// The name to reference this mapping by (from `as` argument, or defaults to type_name)
    pub alias: Name,

    /// The explicit selection string, if provided.
    /// If None, auto-mapping should be used (map all fields by their names).
    pub selection: Option<String>,

    /// Field names from the type, used for auto-mapping when selection is None
    pub field_names: Vec<Name>,
}

/// Extract all `@mapping` directive arguments from the schema
pub(crate) fn extract_mapping_directive_arguments(
    schema: &Schema,
    directive_name: &Name,
) -> Result<Vec<MappingDirectiveArguments>, FederationError> {
    let connect_spec = connect_spec_from_schema(schema).unwrap_or(DEFAULT_CONNECT_SPEC);

    // Only allow @mapping in V0_5+
    if connect_spec < ConnectSpec::V0_5 {
        // Return empty list for older versions - @mapping is not supported
        return Ok(Vec::new());
    }

    let mut results = Vec::new();

    // Iterate over Object types
    for (type_name, ty) in &schema.types {
        if let apollo_compiler::schema::ExtendedType::Object(object_type) = ty {
            let field_names: Vec<Name> = object_type.fields.keys().cloned().collect();

            for directive in object_type
                .directives
                .iter()
                .filter(|d| d.name == *directive_name)
            {
                results.push(extract_single_mapping(
                    type_name.clone(),
                    directive,
                    &field_names,
                )?);
            }
        }

        // Also handle Interface types
        if let apollo_compiler::schema::ExtendedType::Interface(interface_type) = ty {
            let field_names: Vec<Name> = interface_type.fields.keys().cloned().collect();

            for directive in interface_type
                .directives
                .iter()
                .filter(|d| d.name == *directive_name)
            {
                results.push(extract_single_mapping(
                    type_name.clone(),
                    directive,
                    &field_names,
                )?);
            }
        }
    }

    Ok(results)
}

/// Extract a single `@mapping` directive's arguments
fn extract_single_mapping(
    type_name: Name,
    directive: &Node<Directive>,
    field_names: &[Name],
) -> Result<MappingDirectiveArguments, FederationError> {
    let mut selection: Option<String> = None;
    let mut alias: Option<Name> = None;

    for arg in &directive.arguments {
        let arg_name = arg.name.as_str();

        if arg_name == MAPPING_AS_ARGUMENT_NAME.as_str() {
            let as_value = arg.value.as_str().ok_or_else(|| {
                FederationError::internal(format!(
                    "`as` argument in `@mapping` directive on type `{type_name}` is not a string"
                ))
            })?;
            alias = Some(Name::new(as_value)?);
        } else if arg_name == "selection" {
            if let Some(selection_value) = arg.value.as_str() {
                selection = Some(selection_value.to_string());
            }
        }
        // Unknown arguments are silently ignored (schema validation catches these)
    }

    // If no alias provided, use the type name
    let alias = alias.unwrap_or_else(|| type_name.clone());

    Ok(MappingDirectiveArguments {
        type_name,
        alias,
        selection,
        field_names: field_names.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::name;

    use super::*;

    #[test]
    fn test_auto_mapping_no_args() {
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type User @mapping {
                id: ID!
                name: String!
                email: String!
            }

            type Query {
                user: User
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let mappings = extract_mapping_directive_arguments(&schema, &name!(mapping)).unwrap();
        assert_eq!(mappings.len(), 1);

        let mapping = &mappings[0];
        assert_eq!(mapping.type_name, name!(User));
        assert_eq!(mapping.alias, name!(User)); // defaults to type name
        assert!(mapping.selection.is_none()); // auto-map mode
        assert!(mapping.field_names.contains(&name!(id)));
        assert!(mapping.field_names.contains(&name!(name)));
        assert!(mapping.field_names.contains(&name!(email)));
    }

    #[test]
    fn test_explicit_selection() {
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type Post @mapping(selection: """
                id
                title
                authorName: author.name
            """) {
                id: ID!
                title: String!
                authorName: String!
            }

            type Query {
                post: Post
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let mappings = extract_mapping_directive_arguments(&schema, &name!(mapping)).unwrap();
        assert_eq!(mappings.len(), 1);

        let mapping = &mappings[0];
        assert_eq!(mapping.type_name, name!(Post));
        assert!(mapping.selection.is_some());
        assert!(mapping
            .selection
            .as_ref()
            .unwrap()
            .contains("authorName: author.name"));
    }

    #[test]
    fn test_with_alias() {
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type User @mapping(as: "BasicUser") {
                id: ID!
                name: String!
            }

            type Query {
                user: User
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let mappings = extract_mapping_directive_arguments(&schema, &name!(mapping)).unwrap();
        assert_eq!(mappings.len(), 1);

        let mapping = &mappings[0];
        assert_eq!(mapping.type_name, name!(User));
        assert_eq!(mapping.alias, name!(BasicUser));
    }

    #[test]
    fn test_multiple_mappings_per_type() {
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type Tax
                @mapping
                @mapping(selection: "amount: tax_amount rate: tax_rate_percentage", as: "TaxV2")
            {
                amount: Float!
                rate: Float!
            }

            type Query {
                tax: Tax
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let mappings = extract_mapping_directive_arguments(&schema, &name!(mapping)).unwrap();
        assert_eq!(mappings.len(), 2);

        // Find the auto-map one
        let auto_mapping = mappings.iter().find(|m| m.alias == name!(Tax)).unwrap();
        assert!(auto_mapping.selection.is_none());

        // Find the explicit one
        let explicit_mapping = mappings.iter().find(|m| m.alias == name!(TaxV2)).unwrap();
        assert!(explicit_mapping.selection.is_some());
        assert!(explicit_mapping
            .selection
            .as_ref()
            .unwrap()
            .contains("tax_amount"));
    }

    #[test]
    fn test_mapping_on_interface() {
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            interface Node @mapping {
                id: ID!
            }

            type User implements Node {
                id: ID!
                name: String!
            }

            type Query { node: Node }
            "#,
            "test.graphql",
        )
        .unwrap();

        let mappings = extract_mapping_directive_arguments(&schema, &name!(mapping)).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].type_name, name!(Node));
    }

    #[test]
    fn test_mapping_ignored_in_v0_4() {
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.4", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type User @mapping { id: ID! }
            type Query { user: User }
            "#,
            "test.graphql",
        )
        .unwrap();

        let mappings = extract_mapping_directive_arguments(&schema, &name!(mapping)).unwrap();
        // Should return empty because @mapping requires v0.5+
        assert!(mappings.is_empty());
    }
}
