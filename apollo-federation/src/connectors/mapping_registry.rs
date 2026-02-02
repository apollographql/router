//! MappingRegistry for storing and expanding @mapping directive definitions.
//!
//! This module provides the `MappingRegistry` which stores parsed mapping definitions
//! and can expand `...TypeName` spread syntax in JSONSelection strings.

use std::collections::HashSet;

/// Maximum depth for mapping expansion to prevent stack overflow on deeply nested chains.
/// This is intentionally conservative - typical use cases have 1-3 levels of nesting.
const MAX_EXPANSION_DEPTH: usize = 32;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use indexmap::IndexMap;

use super::ConnectSpec;
use super::JSONSelection;
use super::json_selection::NamingPrefix;
use super::json_selection::NamedSelection;
use super::json_selection::PathList;
use super::json_selection::PathSelection;
use super::json_selection::Ranged;
use super::json_selection::SubSelection;
use super::json_selection::TopLevelSelection;
use super::json_selection::WithRange;
use super::spec::ConnectLink;
use super::spec::MappingDirectiveArguments;
use super::spec::extract_mapping_directive_arguments;
use crate::error::FederationError;

/// A parsed mapping definition from a `@mapping` directive
#[derive(Debug, Clone)]
pub struct MappingDefinition {
    /// The parsed selection for this mapping
    pub selection: SubSelection,
    /// The original GraphQL type this mapping is defined on
    pub source_type: Name,
}

/// Registry of all @mapping definitions in a schema
#[derive(Debug, Clone, Default)]
pub struct MappingRegistry {
    /// Mappings keyed by their alias (or type name if no alias)
    mappings: IndexMap<Name, MappingDefinition>,
}

impl MappingRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from a schema by extracting all @mapping directives
    pub fn from_schema(schema: &Schema) -> Result<Self, FederationError> {
        // Get the mapping directive name and spec from the ConnectLink
        let (directive_name, spec) = match ConnectLink::new(schema) {
            Some(Ok(link)) => (link.mapping_directive_name.clone(), link.spec),
            Some(Err(e)) => {
                // Propagate errors from ConnectLink creation (e.g., unknown spec version)
                return Err(FederationError::internal(e.message));
            }
            None => {
                // No connect link at all, return empty registry
                return Ok(Self::default());
            }
        };

        let mut registry = Self::new();

        // Extract all @mapping directive arguments
        let mapping_args = extract_mapping_directive_arguments(schema, &directive_name)?;

        for args in mapping_args {
            let definition = Self::build_mapping_definition(&args, spec)?;
            registry.mappings.insert(args.alias.clone(), definition);
        }

        Ok(registry)
    }

    /// Build a MappingDefinition from directive arguments
    fn build_mapping_definition(
        args: &MappingDirectiveArguments,
        spec: ConnectSpec,
    ) -> Result<MappingDefinition, FederationError> {
        let selection = if let Some(selection_str) = &args.selection {
            // Explicit selection - parse it using the schema's actual spec version
            let parsed = JSONSelection::parse_with_spec(selection_str, spec).map_err(|e| {
                FederationError::internal(format!(
                    "Failed to parse @mapping selection on type `{}`: {}",
                    args.type_name, e
                ))
            })?;

            // Extract the SubSelection from the parsed result
            match parsed.inner {
                TopLevelSelection::Named(sub) => sub,
                TopLevelSelection::Path(_) => {
                    return Err(FederationError::internal(format!(
                        "@mapping selection on type `{}` must be a field selection, not a path",
                        args.type_name
                    )));
                }
            }
        } else {
            // Auto-map mode: generate selection from field names
            Self::generate_auto_map_selection(&args.field_names, spec)?
        };

        Ok(MappingDefinition {
            selection,
            source_type: args.type_name.clone(),
        })
    }

    /// Generate an auto-map SubSelection from field names
    fn generate_auto_map_selection(
        field_names: &[Name],
        spec: ConnectSpec,
    ) -> Result<SubSelection, FederationError> {
        // Generate a simple selection string like "field1 field2 field3"
        let selection_str = field_names
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        if selection_str.is_empty() {
            return Err(FederationError::internal(
                "@mapping on type with no fields".to_string(),
            ));
        }

        let parsed = JSONSelection::parse_with_spec(&selection_str, spec).map_err(|e| {
            FederationError::internal(format!("Failed to generate auto-map selection: {}", e))
        })?;

        match parsed.inner {
            TopLevelSelection::Named(sub) => Ok(sub),
            TopLevelSelection::Path(_) => Err(FederationError::internal(
                "Auto-map generated unexpected path selection".to_string(),
            )),
        }
    }

    /// Check if the registry has a mapping with the given name
    pub fn has_mapping(&self, name: &str) -> bool {
        self.mappings.contains_key(name)
    }

    /// Get a mapping by name
    pub fn get_mapping(&self, name: &str) -> Option<&MappingDefinition> {
        self.mappings.get(name)
    }

    /// Expand all `...TypeName` spreads in a JSONSelection
    ///
    /// This replaces `SpreadNamed` nodes with the corresponding mapping's selection.
    /// Handles recursive spreads and detects circular references.
    pub fn expand_selection(
        &self,
        selection: &JSONSelection,
    ) -> Result<JSONSelection, FederationError> {
        let mut expanding: HashSet<String> = HashSet::new();
        let expanded_inner = self.expand_top_level(&selection.inner, &mut expanding, 0)?;

        Ok(JSONSelection {
            inner: expanded_inner,
            spec: selection.spec,
        })
    }

    /// Expand a TopLevelSelection
    fn expand_top_level(
        &self,
        top_level: &TopLevelSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
    ) -> Result<TopLevelSelection, FederationError> {
        if depth > MAX_EXPANSION_DEPTH {
            return Err(FederationError::internal(format!(
                "Mapping expansion exceeded maximum depth of {}. \
                 This may indicate an overly complex mapping chain.",
                MAX_EXPANSION_DEPTH
            )));
        }

        match top_level {
            TopLevelSelection::Named(sub) => {
                let expanded = self.expand_sub_selection(sub, expanding, depth)?;
                Ok(TopLevelSelection::Named(expanded))
            }
            TopLevelSelection::Path(path) => {
                let expanded = self.expand_path_selection(path, expanding, depth)?;
                Ok(TopLevelSelection::Path(expanded))
            }
        }
    }

    /// Expand a SubSelection, replacing any SpreadNamed nodes
    fn expand_sub_selection(
        &self,
        sub: &SubSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
    ) -> Result<SubSelection, FederationError> {
        let mut new_selections = Vec::new();

        for named in &sub.selections {
            match &named.prefix {
                NamingPrefix::SpreadNamed { name, .. } => {
                    let type_name = name.as_ref();

                    // Check for circular reference
                    if expanding.contains(type_name) {
                        return Err(FederationError::internal(format!(
                            "Circular reference detected in @mapping: ...{} references itself",
                            type_name
                        )));
                    }

                    // Look up the mapping
                    if let Some(mapping) = self.get_mapping(type_name) {
                        // Mark as expanding to detect cycles
                        expanding.insert(type_name.to_string());

                        // Recursively expand the mapping's selection (increment depth)
                        let expanded =
                            self.expand_sub_selection(&mapping.selection, expanding, depth + 1)?;

                        // Remove from expanding set
                        expanding.remove(type_name);

                        // Add all selections from the expanded mapping
                        new_selections.extend(expanded.selections);
                    } else {
                        return Err(FederationError::internal(format!(
                            "Unknown mapping reference: ...{}. \
                             Make sure a @mapping directive is defined on type `{}`.",
                            type_name, type_name
                        )));
                    }
                }
                _ => {
                    // Recursively expand any nested selections
                    let expanded_named = self.expand_named_selection(named, expanding, depth)?;
                    new_selections.push(expanded_named);
                }
            }
        }

        Ok(SubSelection {
            selections: new_selections,
            range: sub.range.clone(),
        })
    }

    /// Expand a NamedSelection, recursively expanding any nested selections
    fn expand_named_selection(
        &self,
        named: &NamedSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
    ) -> Result<NamedSelection, FederationError> {
        let expanded_path = self.expand_path_selection(&named.path, expanding, depth)?;

        Ok(NamedSelection {
            prefix: named.prefix.clone(),
            path: expanded_path,
        })
    }

    /// Expand a PathSelection, recursively expanding any nested SubSelections
    fn expand_path_selection(
        &self,
        path: &PathSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
    ) -> Result<PathSelection, FederationError> {
        let expanded_path_list = self.expand_path_list(path.path.as_ref(), expanding, depth)?;

        Ok(PathSelection {
            path: WithRange::new(expanded_path_list, path.path.range()),
        })
    }

    /// Expand a PathList, recursively expanding any nested SubSelections
    fn expand_path_list(
        &self,
        path_list: &PathList,
        expanding: &mut HashSet<String>,
        depth: usize,
    ) -> Result<PathList, FederationError> {
        match path_list {
            PathList::Selection(sub) => {
                let expanded = self.expand_sub_selection(sub, expanding, depth)?;
                Ok(PathList::Selection(expanded))
            }
            PathList::Key(key, tail) => {
                let expanded_tail = self.expand_path_list(tail.as_ref(), expanding, depth)?;
                Ok(PathList::Key(
                    key.clone(),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Var(var, tail) => {
                let expanded_tail = self.expand_path_list(tail.as_ref(), expanding, depth)?;
                Ok(PathList::Var(
                    var.clone(),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Method(method, args, tail) => {
                let expanded_tail = self.expand_path_list(tail.as_ref(), expanding, depth)?;
                Ok(PathList::Method(
                    method.clone(),
                    args.clone(),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Expr(expr, tail) => {
                let expanded_tail = self.expand_path_list(tail.as_ref(), expanding, depth)?;
                Ok(PathList::Expr(
                    expr.clone(),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Question(tail) => {
                let expanded_tail = self.expand_path_list(tail.as_ref(), expanding, depth)?;
                Ok(PathList::Question(WithRange::new(
                    expanded_tail,
                    tail.range(),
                )))
            }
            PathList::Empty => Ok(PathList::Empty),
        }
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    /// Get the number of mappings in the registry
    pub fn len(&self) -> usize {
        self.mappings.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::json_selection::PrettyPrintable;

    #[test]
    fn test_auto_map_selection_generation() {
        use apollo_compiler::name;

        let field_names = vec![name!(id), name!(name), name!(email)];
        let sub =
            MappingRegistry::generate_auto_map_selection(&field_names, ConnectSpec::V0_5).unwrap();

        assert_eq!(sub.selections.len(), 3);
    }

    #[test]
    fn test_methods_allowed_in_mapping_selection() {
        use apollo_compiler::name;

        // Methods ARE now allowed in @mapping selections
        let args = MappingDirectiveArguments {
            type_name: name!(User),
            alias: name!(User),
            selection: Some("id name: fullName->echo".to_string()),
            field_names: vec![name!(id), name!(name)],
        };

        let result = MappingRegistry::build_mapping_definition(&args, ConnectSpec::V0_5);
        assert!(
            result.is_ok(),
            "Methods should be allowed in @mapping selections"
        );

        // Verify the selection was parsed correctly
        let definition = result.unwrap();
        assert_eq!(definition.selection.selections.len(), 2);
    }

    #[test]
    fn test_spread_named_with_method_is_separate_token() {
        // Verify that ...TypeName->method() parses as TWO separate things:
        // 1. SpreadNamed "User"
        // 2. Unparseable remainder "->method()"
        //
        // The parser creates SpreadNamed with path: PathSelection::empty(),
        // so ->method() cannot attach to it. It becomes the remainder which
        // fails to parse as a valid next selection.

        let input = "...User->first()";
        let result = JSONSelection::parse_with_spec(input, ConnectSpec::V0_5);

        // This should fail because "->first()" is not a valid selection start
        assert!(
            result.is_err(),
            "...Type->method() should fail to parse: {:?}",
            result
        );
    }

    #[test]
    fn test_expand_simple_spread() {
        use apollo_compiler::name;

        // Create a registry with a User mapping
        let mut registry = MappingRegistry::new();
        let user_selection =
            JSONSelection::parse_with_spec("id name email", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(User),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Parse a selection with ...User
        let selection = JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        // Verify the expansion
        assert_eq!(expanded.pretty_print(), "id\nname\nemail");
    }

    #[test]
    fn test_expand_spread_with_additional_fields() {
        use apollo_compiler::name;

        // Create a registry with a User mapping
        let mut registry = MappingRegistry::new();
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(User),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Parse a selection with ...User and extra field
        let selection =
            JSONSelection::parse_with_spec("...User extraField", ConnectSpec::V0_5).unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        // Verify the expansion includes both the User fields and the extra field
        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        assert!(pretty.contains("extraField"));
    }

    #[test]
    fn test_circular_reference_detection() {
        use apollo_compiler::name;

        // Create a registry with circular references
        let mut registry = MappingRegistry::new();

        // UserA references UserB
        let user_a_selection =
            JSONSelection::parse_with_spec("id ...UserB", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_a_selection.inner {
            registry.mappings.insert(
                name!(UserA),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(UserA),
                },
            );
        }

        // UserB references UserA (circular!)
        let user_b_selection =
            JSONSelection::parse_with_spec("name ...UserA", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_b_selection.inner {
            registry.mappings.insert(
                name!(UserB),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(UserB),
                },
            );
        }

        // Try to expand UserA - should fail with circular reference error
        let selection = JSONSelection::parse_with_spec("...UserA", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Circular reference"));
    }

    #[test]
    fn test_unknown_mapping_error() {
        let registry = MappingRegistry::new();

        // Try to expand a spread that references a non-existent mapping
        let selection =
            JSONSelection::parse_with_spec("...UnknownType", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown mapping"));
    }

    #[test]
    fn test_nested_spread_expansion() {
        use apollo_compiler::name;

        // Create a registry with nested mappings
        let mut registry = MappingRegistry::new();

        // Address mapping
        let address_selection =
            JSONSelection::parse_with_spec("street city zipCode", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = address_selection.inner {
            registry.mappings.insert(
                name!(Address),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(Address),
                },
            );
        }

        // User mapping references Address
        let user_selection =
            JSONSelection::parse_with_spec("id name address { ...Address }", ConnectSpec::V0_5)
                .unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(User),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Expand User
        let selection = JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        assert!(pretty.contains("address"));
        assert!(pretty.contains("street"));
        assert!(pretty.contains("city"));
        assert!(pretty.contains("zipCode"));
    }

    #[test]
    fn test_spread_with_alias() {
        use apollo_compiler::name;

        // Create a registry with a mapping using alias
        let mut registry = MappingRegistry::new();

        // Create User mapping aliased as "BasicUser"
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(BasicUser),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Expand using alias
        let selection = JSONSelection::parse_with_spec("...BasicUser", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
    }

    #[test]
    fn test_spread_preserves_other_selections() {
        use apollo_compiler::name;

        // Create a registry with a User mapping
        let mut registry = MappingRegistry::new();
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(User),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Parse a selection with spread + other fields + nested selection
        let selection = JSONSelection::parse_with_spec(
            "...User email posts { title content }",
            ConnectSpec::V0_5,
        )
        .unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        // Spread fields
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        // Additional field
        assert!(pretty.contains("email"));
        // Nested selection
        assert!(pretty.contains("posts"));
        assert!(pretty.contains("title"));
        assert!(pretty.contains("content"));
    }

    #[test]
    fn test_expand_path_selection_with_spread() {
        use apollo_compiler::name;

        // Create a registry with a mapping
        let mut registry = MappingRegistry::new();
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(User),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Parse a selection with path containing a nested subselection with spread
        let selection =
            JSONSelection::parse_with_spec("users: $.data.users { ...User }", ConnectSpec::V0_5)
                .unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("users:"));
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
    }

    #[test]
    fn test_multiple_spreads_same_selection() {
        use apollo_compiler::name;

        // Create a registry with multiple mappings
        let mut registry = MappingRegistry::new();

        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = user_selection.inner {
            registry.mappings.insert(
                name!(UserBasic),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        let contact_selection =
            JSONSelection::parse_with_spec("email phone", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = contact_selection.inner {
            registry.mappings.insert(
                name!(ContactInfo),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        // Parse a selection with multiple spreads
        let selection =
            JSONSelection::parse_with_spec("...UserBasic ...ContactInfo", ConnectSpec::V0_5)
                .unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        assert!(pretty.contains("email"));
        assert!(pretty.contains("phone"));
    }

    #[test]
    fn test_from_schema_integration() {
        use apollo_compiler::Schema;

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

            type Product @mapping(selection: "sku: product_sku title: product_title", as: "ProductV2") {
                sku: ID!
                title: String!
            }

            type Query {
                users: [User]
                products: [Product]
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let registry = MappingRegistry::from_schema(&schema).unwrap();

        // Auto-mapped User should be in registry
        assert!(registry.has_mapping("User"));

        // Aliased mapping should be in registry
        assert!(registry.has_mapping("ProductV2"));

        // Original Product type should NOT have a separate mapping (only ProductV2)
        assert!(!registry.has_mapping("Product"));
    }

    #[test]
    fn test_auto_map_empty_fields_error() {
        // Auto-map with no fields should fail
        let result = MappingRegistry::generate_auto_map_selection(&[], ConnectSpec::V0_5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no fields"));
    }

    #[test]
    fn test_invalid_selection_syntax_error() {
        use apollo_compiler::name;

        let args = MappingDirectiveArguments {
            type_name: name!(User),
            alias: name!(User),
            selection: Some("{ invalid [ syntax".to_string()),
            field_names: vec![name!(id)],
        };

        let result = MappingRegistry::build_mapping_definition(&args, ConnectSpec::V0_5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn test_path_selection_not_allowed_in_mapping() {
        use apollo_compiler::name;

        // Path selection (starting with $) is not allowed in @mapping
        let args = MappingDirectiveArguments {
            type_name: name!(User),
            alias: name!(User),
            selection: Some("$.data.id".to_string()),
            field_names: vec![name!(id)],
        };

        let result = MappingRegistry::build_mapping_definition(&args, ConnectSpec::V0_5);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be a field selection, not a path"));
    }

    #[test]
    fn test_duplicate_alias_last_wins() {
        use apollo_compiler::name;

        // When two mappings have the same alias, the last one wins (IndexMap behavior)
        let mut registry = MappingRegistry::new();

        let selection1 = JSONSelection::parse_with_spec("id", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = selection1.inner {
            registry.mappings.insert(
                name!(UserMapping),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(User),
                },
            );
        }

        let selection2 =
            JSONSelection::parse_with_spec("id name email", ConnectSpec::V0_5).unwrap();
        if let TopLevelSelection::Named(sub) = selection2.inner {
            registry.mappings.insert(
                name!(UserMapping),
                MappingDefinition {
                    selection: sub,
                    source_type: name!(Admin),
                },
            );
        }

        assert_eq!(registry.len(), 1);
        let mapping = registry.get_mapping("UserMapping").unwrap();
        assert_eq!(mapping.source_type, name!(Admin));
    }

    #[test]
    fn test_both_spread_types_together() {
        // Test that both anonymous spread (...path) and named spread (...TypeName) work
        // Named spread (uppercase) references a @mapping
        let named_spread = JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let named_pretty = named_spread.pretty_print();
        assert!(
            named_pretty.contains("...User"),
            "Named spread ...User not found in: {}",
            named_pretty
        );

        // Anonymous spread (lowercase path) spreads a path into the result
        // Syntax is `...path` or `...path { subfields }`
        // Note: pretty print outputs `... metadata` (with space after `...`)
        let anon_spread =
            JSONSelection::parse_with_spec("...metadata { id name }", ConnectSpec::V0_5).unwrap();
        let anon_pretty = anon_spread.pretty_print();
        assert!(
            anon_pretty.contains("... metadata"),
            "Anonymous spread ... metadata not found in: {}",
            anon_pretty
        );
    }
}
