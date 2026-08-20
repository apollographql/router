use std::ops::Range;

use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::ExtendedType;

use crate::error::SubgraphLocation;
use crate::merger::hints::DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE;
use crate::merger::hints::DEPRECATED_REASON_NULL;
use crate::supergraph::CompositionHint;

/// Upgrade a federation 2 subgraph schema so it is valid under the GraphQL
/// September 2025 spec (federation 3 / Router 3). Returns composition hints
/// describing every transformation applied.
///
/// Handles:
/// 1. `@deprecated(reason: null)` — the `reason` argument became non-nullable
///    in the 2025 spec. The `reason: null` argument is stripped, leaving a bare
///    `@deprecated` (which receives the default reason "No longer supported").
///    This applies wherever `@deprecated` is valid: object/interface fields,
///    enum values, input fields, and argument definitions.
/// 2. `@deprecated` on an implementing field whose corresponding interface
///    field is *not* deprecated — disallowed by the 2025 spec. The directive
///    is stripped from the implementing field.
pub(crate) fn apply_fed3_upgrade(
    schema: &mut Schema,
    subgraph_name: &str,
) -> Vec<CompositionHint> {
    let mut hints = Vec::new();

    // Clone the source map before mutating types — it's Arc-backed so this is cheap.
    let sources = schema.sources.clone();

    // Collect interface field deprecation status up front so we aren't
    // borrowing the schema while mutating it.
    let iface_deprecated = collect_interface_deprecated_fields(schema);

    for (type_name, ty) in &mut schema.types {
        match ty {
            ExtendedType::Object(object) => {
                let object = object.make_mut();
                upgrade_fields_and_args(&mut hints, &mut object.fields, type_name, subgraph_name, &sources);
                strip_deprecated_on_non_deprecated_interface_fields(
                    &mut hints, &object.implements_interfaces, &mut object.fields,
                    &object.name, &iface_deprecated, subgraph_name, &sources,
                );
            }
            ExtendedType::Interface(interface) => {
                let interface = interface.make_mut();
                upgrade_fields_and_args(&mut hints, &mut interface.fields, type_name, subgraph_name, &sources);
                strip_deprecated_on_non_deprecated_interface_fields(
                    &mut hints, &interface.implements_interfaces, &mut interface.fields,
                    &interface.name, &iface_deprecated, subgraph_name, &sources,
                );
            }
            ExtendedType::Enum(enum_type) => {
                let enum_type = enum_type.make_mut();
                for value in enum_type.values.values_mut() {
                    let value_name = value.value.clone();
                    let value_def = value.make_mut();
                    strip_deprecated_reason_null(
                        &mut hints, &mut value_def.directives,
                        type_name, &value_name, subgraph_name, &sources,
                    );
                }
            }
            ExtendedType::InputObject(input) => {
                let input = input.make_mut();
                for field in input.fields.values_mut() {
                    let field_name = field.name.clone();
                    let field_def = field.make_mut();
                    strip_deprecated_reason_null(
                        &mut hints, &mut field_def.directives,
                        type_name, &field_name, subgraph_name, &sources,
                    );
                }
            }
            _ => {}
        }
    }
    hints
}

/// Strip `@deprecated(reason: null)` from all fields and their arguments.
fn upgrade_fields_and_args(
    hints: &mut Vec<CompositionHint>,
    fields: &mut IndexMap<apollo_compiler::Name, Component<apollo_compiler::ast::FieldDefinition>>,
    type_name: &apollo_compiler::Name,
    subgraph_name: &str,
    sources: &SourceMap,
) {
    for field in fields.values_mut() {
        let field_name = field.name.clone();
        let field_def = field.make_mut();
        strip_deprecated_reason_null(
            hints, &mut field_def.directives,
            type_name, &field_name, subgraph_name, sources,
        );
        for arg in &mut field_def.arguments {
            let arg_name = arg.name.clone();
            let arg_def = arg.make_mut();
            strip_deprecated_reason_null(
                hints, &mut arg_def.directives,
                type_name, &arg_name, subgraph_name, sources,
            );
        }
    }
}

/// For each interface this type implements, strip `@deprecated` from fields
/// where the corresponding interface field is not deprecated.
fn strip_deprecated_on_non_deprecated_interface_fields(
    hints: &mut Vec<CompositionHint>,
    implements_interfaces: &IndexSet<ComponentName>,
    fields: &mut IndexMap<apollo_compiler::Name, Component<apollo_compiler::ast::FieldDefinition>>,
    type_name: &apollo_compiler::Name,
    iface_deprecated: &IndexMap<apollo_compiler::Name, IndexSet<apollo_compiler::Name>>,
    subgraph_name: &str,
    sources: &SourceMap,
) {
    for iface_name in implements_interfaces {
        if let Some(deprecated_fields) = iface_deprecated.get(&iface_name.name) {
            for field in fields.values_mut() {
                let field_name = field.name.clone();
                if !deprecated_fields.contains(&field_name) {
                    let field_def = field.make_mut();
                    strip_deprecated_without_interface(
                        hints, &mut field_def.directives,
                        type_name, &field_name, &iface_name.name,
                        subgraph_name, sources,
                    );
                }
            }
        }
    }
}

/// Returns a map from interface name to the set of deprecated field names
/// for every interface type in the schema.
fn collect_interface_deprecated_fields(
    schema: &Schema,
) -> IndexMap<apollo_compiler::Name, IndexSet<apollo_compiler::Name>> {
    let mut result = IndexMap::default();
    for ty in schema.types.values() {
        if let ExtendedType::Interface(interface) = ty {
            let deprecated_fields: IndexSet<_> = interface
                .fields
                .iter()
                .filter(|(_, field)| has_deprecated(&field.directives))
                .map(|(name, _)| name.clone())
                .collect();
            result.insert(interface.name.clone(), deprecated_fields);
        }
    }
    result
}

/// Returns true if the directive list contains any `@deprecated` application.
fn has_deprecated(directives: &apollo_compiler::ast::DirectiveList) -> bool {
    directives.0.iter().any(|d| d.name == "deprecated")
}

fn default_range() -> Range<LineColumn> {
    LineColumn { line: 0, column: 0 }..LineColumn { line: 0, column: 0 }
}

/// Strip the `reason: null` argument from `@deprecated(reason: null)`,
/// leaving a bare `@deprecated` (which receives the default reason
/// "No longer supported"). Emits a hint for each occurrence.
fn strip_deprecated_reason_null(
    hints: &mut Vec<CompositionHint>,
    directives: &mut apollo_compiler::ast::DirectiveList,
    type_name: &apollo_compiler::Name,
    member_name: &apollo_compiler::Name,
    subgraph_name: &str,
    sources: &SourceMap,
) {
    for directive in directives.0.iter_mut() {
        if directive.name != "deprecated" {
            continue;
        }
        let has_reason_null = directive
            .specified_argument_by_name("reason")
            .is_some_and(|v| v.is_null());
        if has_reason_null {
            let range = directive
                .line_column_range(sources)
                .unwrap_or_else(default_range);
            let directive = directive.make_mut();
            directive.arguments.retain(|arg| arg.name != "reason");
            hints.push(CompositionHint {
                definition: &DEPRECATED_REASON_NULL,
                message: format!(
                    "Stripped `reason: null` from `@deprecated` on `{type_name}.{member_name}` \
                     for federation 3 compatibility. Use a non-null reason or omit the argument."
                ),
                locations: vec![SubgraphLocation {
                    subgraph: subgraph_name.to_owned(),
                    range,
                }],
            });
        }
    }
}

/// Strip `@deprecated` from an implementing field when the corresponding
/// interface field is not deprecated, emitting a hint for each occurrence.
fn strip_deprecated_without_interface(
    hints: &mut Vec<CompositionHint>,
    directives: &mut apollo_compiler::ast::DirectiveList,
    type_name: &apollo_compiler::Name,
    field_name: &apollo_compiler::Name,
    interface_name: &apollo_compiler::Name,
    subgraph_name: &str,
    sources: &SourceMap,
) {
    let deprecated = directives.0.iter().find(|d| d.name == "deprecated");
    if let Some(directive) = deprecated {
        let range = directive
            .line_column_range(sources)
            .unwrap_or_else(default_range);
        directives.0.retain(|d| d.name != "deprecated");
        hints.push(CompositionHint {
            definition: &DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE,
            message: format!(
                "Stripped `@deprecated` from `{type_name}.{field_name}` because \
                 `{interface_name}.{field_name}` is not deprecated. Either deprecate \
                 the interface field or remove `@deprecated` from the implementation."
            ),
            locations: vec![SubgraphLocation {
                subgraph: subgraph_name.to_owned(),
                range,
            }],
        });
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::coord;

    use crate::composition::CompositionOptions;
    use crate::subgraph::typestate::Subgraph;
    use crate::supergraph::Supergraph;

    fn compose_supergraph(sdl: &str) -> (Supergraph<crate::supergraph::Satisfiable>, Vec<String>) {
        let subgraph = Subgraph::parse("test", "http://localhost", sdl).unwrap();
        let result =
            crate::composition::compose(vec![subgraph], CompositionOptions::default()).unwrap();
        let hints: Vec<_> = result.hints().iter().map(|h| h.code().to_string()).collect();
        (result, hints)
    }

    #[test]
    fn strips_deprecated_reason_null() {
        let sdl = r#"
            type Query {
                field: String @deprecated(reason: null)
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            hints.contains(&"DEPRECATED_REASON_NULL".to_string()),
            "Expected DEPRECATED_REASON_NULL hint, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let c = coord!(Query.field);
        let field = schema.type_field(&c.ty, &c.attribute).unwrap();
        assert!(
            field.directives.has("deprecated"),
            "Field should still have @deprecated (only reason: null stripped)"
        );
        let deprecated = field.directives.get("deprecated").unwrap();
        assert!(
            deprecated.specified_argument_by_name("reason").is_none(),
            "reason argument should have been stripped"
        );
    }

    #[test]
    fn preserves_deprecated_with_valid_reason() {
        let sdl = r#"
            type Query {
                field: String @deprecated(reason: "use newField")
                newField: String
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            !hints.contains(&"DEPRECATED_REASON_NULL".to_string()),
            "Should not emit DEPRECATED_REASON_NULL, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let c = coord!(Query.field);
        let field = schema.type_field(&c.ty, &c.attribute).unwrap();
        let deprecated = field.directives.get("deprecated").unwrap();
        assert!(
            deprecated.specified_argument_by_name("reason").is_some(),
            "reason argument should be preserved for valid reasons"
        );
    }

    #[test]
    fn strips_deprecated_implementing_field_without_interface() {
        let sdl = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable"])

            type Query {
                node: Node
            }

            interface Node {
                id: ID!
                name: String
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                name: String @deprecated(reason: "use displayName")
                displayName: String
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            hints.contains(&"DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE".to_string()),
            "Expected DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE hint, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let c = coord!(User.name);
        let field = schema.type_field(&c.ty, &c.attribute).unwrap();
        assert!(
            !field.directives.has("deprecated"),
            "@deprecated should have been stripped from User.name"
        );
    }

    #[test]
    fn allows_deprecated_implementing_field_when_interface_also_deprecated() {
        let sdl = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@shareable"])

            type Query {
                node: Node
            }

            interface Node {
                id: ID!
                name: String @deprecated(reason: "use displayName")
            }

            type User implements Node @key(fields: "id") {
                id: ID!
                name: String @deprecated(reason: "use displayName")
                displayName: String
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            !hints.contains(&"DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE".to_string()),
            "Should not emit DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let c = coord!(User.name);
        let field = schema.type_field(&c.ty, &c.attribute).unwrap();
        assert!(
            field.directives.has("deprecated"),
            "@deprecated should be preserved when interface field is also deprecated"
        );
    }

    #[test]
    fn strips_deprecated_reason_null_on_enum_value() {
        let sdl = r#"
            type Query {
                status: Status
            }

            enum Status {
                ACTIVE
                INACTIVE @deprecated(reason: null)
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            hints.contains(&"DEPRECATED_REASON_NULL".to_string()),
            "Expected DEPRECATED_REASON_NULL hint for enum value, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let status = schema.types.get("Status").unwrap();
        if let apollo_compiler::schema::ExtendedType::Enum(e) = status {
            let inactive = e.values.get("INACTIVE").unwrap();
            assert!(
                inactive.directives.has("deprecated"),
                "Enum value should still have @deprecated"
            );
            let deprecated = inactive.directives.get("deprecated").unwrap();
            assert!(
                deprecated.specified_argument_by_name("reason").is_none(),
                "reason argument should have been stripped from enum value"
            );
        } else {
            panic!("Status should be an enum type");
        }
    }

    #[test]
    fn strips_deprecated_reason_null_on_input_field() {
        let sdl = r#"
            type Query {
                search(filter: Filter): String
            }

            input Filter {
                name: String
                legacyId: ID @deprecated(reason: null)
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            hints.contains(&"DEPRECATED_REASON_NULL".to_string()),
            "Expected DEPRECATED_REASON_NULL hint for input field, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let filter = schema.types.get("Filter").unwrap();
        if let apollo_compiler::schema::ExtendedType::InputObject(input) = filter {
            let field = input.fields.get("legacyId").unwrap();
            assert!(
                field.directives.has("deprecated"),
                "Input field should still have @deprecated"
            );
            let deprecated = field.directives.get("deprecated").unwrap();
            assert!(
                deprecated.specified_argument_by_name("reason").is_none(),
                "reason argument should have been stripped from input field"
            );
        } else {
            panic!("Filter should be an input object type");
        }
    }

    #[test]
    fn strips_deprecated_reason_null_on_argument() {
        let sdl = r#"
            type Query {
                search(query: String, limit: Int @deprecated(reason: null)): [String]
            }
        "#;
        let (supergraph, hints) = compose_supergraph(sdl);
        assert!(
            hints.contains(&"DEPRECATED_REASON_NULL".to_string()),
            "Expected DEPRECATED_REASON_NULL hint for argument, got: {hints:?}"
        );

        let schema = supergraph.schema().schema();
        let c = coord!(Query.search);
        let field = schema.type_field(&c.ty, &c.attribute).unwrap();
        let arg = field
            .arguments
            .iter()
            .find(|a| a.name == "limit")
            .unwrap();
        assert!(
            arg.directives.has("deprecated"),
            "Argument should still have @deprecated"
        );
        let deprecated = arg.directives.get("deprecated").unwrap();
        assert!(
            deprecated.specified_argument_by_name("reason").is_none(),
            "reason argument should have been stripped from argument"
        );
    }
}
