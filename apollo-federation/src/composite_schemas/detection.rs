//! Detecting GraphQL Federation source schemas and normalizing them to federation v2.16.
//!
//! A source schema written against the GraphQL Federation specification carries no `@link` at
//! all. Left alone it would be read as a Federation 1 schema, get Fed 1 definitions injected and
//! skip federation 2 validation, so detection runs on the raw, unexpanded schema, before
//! `expand_links`, and stamps an implicit federation v2.16 `@link` onto it. From then on the
//! schema is an ordinary federation 2.16 subgraph.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;

use crate::link::federation_spec_definition::COMPOSITE_SCHEMAS_FEDERATION_VERSION;
use crate::link::federation_spec_definition::FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::link_spec_definition::LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME;
use crate::link::link_spec_definition::LINK_DIRECTIVE_NAME_IN_SPEC;
use crate::link::link_spec_definition::LINK_DIRECTIVE_URL_ARGUMENT_NAME;
use crate::link::spec_definition::SpecDefinition;
use crate::subgraph::typestate::has_federation_spec_link;

/// Directives that only exist in the GraphQL Federation dialect. Applying any of them is what
/// marks a schema as a source schema.
pub(crate) const COMPOSITE_ONLY_DIRECTIVES: [Name; 4] = [
    FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC,
];

/// Every directive the GraphQL Federation specification defines for source schemas. Source
/// schemas may carry their own definitions of these (copied from the specification); those are
/// replaced by the federation definitions when the schema is normalized.
const SPEC_DIRECTIVES: [Name; 10] = [
    FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC,
    FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC,
    name!("key"),
    name!("shareable"),
    name!("provides"),
    name!("external"),
    name!("override"),
    name!("inaccessible"),
];

/// Scalars the specification uses in its directive definitions.
const SPEC_SCALARS: [Name; 3] = [
    name!("FieldSelectionSet"),
    name!("FieldSelectionMap"),
    name!("FieldSelection"),
];

/// All directive applications in a schema, by name, without expanding anything.
fn applied_directive_names(schema: &Schema) -> impl Iterator<Item = &Name> {
    let schema_level = schema.schema_definition.directives.iter().map(|d| &d.name);
    let types = schema.types.values().flat_map(|ty| {
        let own = ty.directives().iter().map(|d| &d.name);
        let fields: Box<dyn Iterator<Item = &Name>> = match ty {
            ExtendedType::Object(object) => Box::new(object.fields.values().flat_map(|field| {
                field.directives.iter().map(|d| &d.name).chain(
                    field
                        .arguments
                        .iter()
                        .flat_map(|a| a.directives.iter().map(|d| &d.name)),
                )
            })),
            ExtendedType::Interface(interface) => {
                Box::new(interface.fields.values().flat_map(|field| {
                    field.directives.iter().map(|d| &d.name).chain(
                        field
                            .arguments
                            .iter()
                            .flat_map(|a| a.directives.iter().map(|d| &d.name)),
                    )
                }))
            }
            ExtendedType::InputObject(input) => Box::new(
                input
                    .fields
                    .values()
                    .flat_map(|field| field.directives.iter().map(|d| &d.name)),
            ),
            ExtendedType::Enum(enum_) => Box::new(
                enum_
                    .values
                    .values()
                    .flat_map(|value| value.directives.iter().map(|d| &d.name)),
            ),
            _ => Box::new(std::iter::empty()),
        };
        own.chain(fields)
    });
    schema_level.chain(types)
}

/// Whether a user-provided definition of a spec directive is the specification's own (so it can
/// be replaced), rather than an unrelated directive that happens to share the name.
fn is_spec_shaped_definition(schema: &Schema, name: &Name) -> bool {
    let Some(definition) = schema.directive_definitions.get(name) else {
        return true;
    };
    let argument_names: Vec<&str> = definition
        .arguments
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    match name.as_str() {
        "lookup" | "internal" | "shareable" | "inaccessible" => argument_names.is_empty(),
        "is" | "require" => argument_names == ["field"],
        "key" | "provides" => argument_names == ["fields"],
        "external" => argument_names.is_empty() || argument_names == ["reason"],
        "override" => argument_names.first() == Some(&"from"),
        _ => false,
    }
}

/// Whether `schema` is a GraphQL Federation source schema: it has no federation `@link`, and it
/// applies at least one directive that only exists in that dialect (`@lookup`, `@internal`, `@is`,
/// `@require`) without defining it as something else.
///
/// A schema with `@key` but none of those directives stays federation (Fed 1 when unlinked): a
/// federation subgraph always implements `_entities`, so that default is the safe one.
pub(crate) fn is_composite_source_schema(schema: &Schema) -> bool {
    if has_federation_spec_link(schema) {
        return false;
    }
    let mut used = applied_directive_names(schema)
        .filter(|name| COMPOSITE_ONLY_DIRECTIVES.contains(name))
        .peekable();
    used.peek().is_some() && used.all(|name| is_spec_shaped_definition(schema, name))
}

/// Normalize a detected source schema into a federation v2.16 subgraph: drop user copies of the
/// specification's directive and scalar definitions, and add
/// `@link(url: ".../federation/v2.16", import: [...])` importing the specification's directives
/// plus any other federation directive the schema applies unprefixed.
pub(crate) fn stamp_federation_link(schema: &mut Schema) {
    for name in SPEC_DIRECTIVES {
        if is_spec_shaped_definition(schema, &name) {
            schema.directive_definitions.shift_remove(&name);
        }
    }
    for name in SPEC_SCALARS {
        if matches!(schema.types.get(&name), Some(ExtendedType::Scalar(_))) {
            schema.types.shift_remove(&name);
        }
    }

    let federation_spec =
        FederationSpecDefinition::for_version(&COMPOSITE_SCHEMAS_FEDERATION_VERSION)
            .expect("the composite schemas federation version is registered");
    let applied: Vec<Name> = applied_directive_names(schema).cloned().collect();
    let mut imports: Vec<Node<Value>> = Vec::new();
    for spec in federation_spec.directive_specs() {
        let name = spec.name();
        let is_spec_directive = SPEC_DIRECTIVES.contains(name);
        let user_defined = schema.directive_definitions.contains_key(name);
        if user_defined {
            continue;
        }
        if is_spec_directive || applied.contains(name) {
            imports.push(Node::new(Value::String(format!("@{name}"))));
        }
    }
    imports.push(Node::new(Value::String("FieldSelectionMap".to_string())));

    schema
        .schema_definition
        .make_mut()
        .directives
        .push(Component::new(Directive {
            name: LINK_DIRECTIVE_NAME_IN_SPEC,
            arguments: vec![
                Node::new(ast::Argument {
                    name: LINK_DIRECTIVE_URL_ARGUMENT_NAME,
                    value: federation_spec.url().to_string().into(),
                }),
                Node::new(ast::Argument {
                    name: LINK_DIRECTIVE_IMPORT_ARGUMENT_NAME,
                    value: Node::new(Value::List(imports)),
                }),
            ],
        }));
}
