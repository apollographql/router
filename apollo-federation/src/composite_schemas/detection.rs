//! Detecting GraphQL Federation source schemas and normalizing them to federation v3.0.
//!
//! A source schema written against the GraphQL Federation specification carries no `@link` at
//! all. Left alone it would be read as a Federation 1 schema, get Fed 1 definitions injected and
//! skip federation 2 validation, so detection runs on the raw, unexpanded schema, before
//! `expand_links`, and stamps an implicit federation v3.0 `@link` onto it. From then on the
//! schema is an ordinary federation 3.0 subgraph.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
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

/// Directives that only exist in the GraphQL Federation dialect. Applying any of them subjects a
/// subgraph to the GraphQL Federation validation rules and to the composition gate; only applying
/// `@lookup` makes it a source schema (see [`is_composite_source_schema`]).
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

/// All directive applications in a schema, by name: on the schema, on types, on fields and their
/// arguments, on input fields and on enum values.
pub(crate) fn applied_directive_names(schema: &Schema) -> impl Iterator<Item = &Name> {
    fn names(directives: &ast::DirectiveList) -> impl Iterator<Item = &Name> {
        directives.iter().map(|d| &d.name)
    }
    fn field_names(field: &ast::FieldDefinition) -> impl Iterator<Item = &Name> {
        names(&field.directives).chain(field.arguments.iter().flat_map(|a| names(&a.directives)))
    }
    let types = schema.types.values().flat_map(|ty| {
        let members: Box<dyn Iterator<Item = &Name>> = match ty {
            ExtendedType::Object(object) => {
                Box::new(object.fields.values().flat_map(|f| field_names(f)))
            }
            ExtendedType::Interface(interface) => {
                Box::new(interface.fields.values().flat_map(|f| field_names(f)))
            }
            ExtendedType::InputObject(input) => {
                Box::new(input.fields.values().flat_map(|f| names(&f.directives)))
            }
            ExtendedType::Enum(enum_) => {
                Box::new(enum_.values.values().flat_map(|v| names(&v.directives)))
            }
            _ => Box::new(std::iter::empty()),
        };
        ty.directives().iter().map(|d| &d.name).chain(members)
    });
    names(&schema.schema_definition.directives).chain(types)
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
    // The composite directives also have to match on repeatability and locations. Otherwise a
    // Fed 1 schema with its own argument-less `@lookup` would be read as a source schema.
    // Locations only need to be a subset, so a copy of an older spec draft still matches.
    let fits = |allowed: &[DirectiveLocation]| {
        !definition.repeatable
            && definition
                .locations
                .iter()
                .all(|location| allowed.contains(location))
    };
    match name.as_str() {
        "lookup" => argument_names.is_empty() && fits(&[DirectiveLocation::FieldDefinition]),
        "internal" => {
            argument_names.is_empty()
                && fits(&[
                    DirectiveLocation::Object,
                    DirectiveLocation::InputObject,
                    DirectiveLocation::FieldDefinition,
                ])
        }
        "is" | "require" => {
            argument_names == ["field"] && fits(&[DirectiveLocation::ArgumentDefinition])
        }
        "shareable" | "inaccessible" => argument_names.is_empty(),
        "key" | "provides" => argument_names == ["fields"],
        "external" => argument_names.is_empty() || argument_names == ["reason"],
        "override" => argument_names.first() == Some(&"from"),
        _ => false,
    }
}

/// Whether `schema` is a GraphQL Federation source schema: it has no federation `@link`, it
/// applies `@lookup`, and it does not define `@lookup`, `@internal`, `@is` or `@require` as
/// something else.
///
/// A schema without `@lookup` stays federation (Fed 1 when unlinked), even if it applies
/// `@internal`, `@is` or `@require`: a federation subgraph always implements `_entities`, so that
/// default is the safe one.
pub(crate) fn is_composite_source_schema(schema: &Schema) -> bool {
    if has_federation_spec_link(schema) {
        return false;
    }
    let used: Vec<&Name> = applied_directive_names(schema)
        .filter(|name| COMPOSITE_ONLY_DIRECTIVES.contains(name))
        .collect();
    used.contains(&&FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC)
        && used
            .into_iter()
            .all(|name| is_spec_shaped_definition(schema, name))
}

/// Normalize a detected source schema into a federation v3.0 subgraph: drop user copies of the
/// specification's directive definitions (and the specification's scalars, once nothing else
/// references them), and add `@link(url: ".../federation/v3.0", import: [...])` importing:
/// - the specification's directives, unless the schema defines one as something else and does not
///   apply it;
/// - every other federation directive the schema applies unprefixed;
/// - every federation type the schema defines itself (e.g. `scalar FieldSet`).
///
/// A user definition of an imported directive or type is kept, and federation checks that it is
/// compatible, as it does for any linked schema. So a copied Apollo definition such as
/// `directive @key(fields: FieldSet!, resolvable: Boolean = true)` keeps its federation meaning,
/// and an incompatible one is reported instead of silently turning into a custom directive.
pub(crate) fn stamp_federation_link(schema: &mut Schema) {
    for name in SPEC_DIRECTIVES {
        if is_spec_shaped_definition(schema, &name) {
            schema.directive_definitions.shift_remove(&name);
        }
    }
    for name in SPEC_SCALARS {
        if matches!(schema.types.get(&name), Some(ExtendedType::Scalar(_)))
            && !references_type(schema, &name)
        {
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
        let is_applied = applied.contains(name);
        let is_spec_directive = SPEC_DIRECTIVES.contains(name);
        let user_defined = schema.directive_definitions.contains_key(name);
        if is_applied || (is_spec_directive && !user_defined) {
            imports.push(Node::new(Value::String(format!("@{name}"))));
        }
    }
    for spec in federation_spec.type_specs() {
        let name = spec.name();
        if name != "FieldSelectionMap" && schema.types.contains_key(name) {
            imports.push(Node::new(Value::String(name.to_string())));
        }
    }
    imports.push(Node::new(Value::String("FieldSelectionMap".to_string())));

    schema
        .schema_definition
        .make_mut()
        .directives
        .push(Node::new(Directive {
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

/// Whether any definition left in `schema` refers to the type `name`: a directive argument, a
/// field or its arguments, an input field, a union member or an implemented interface.
fn references_type(schema: &Schema, name: &Name) -> bool {
    let in_arguments = |arguments: &[Node<ast::InputValueDefinition>]| {
        arguments.iter().any(|a| a.ty.inner_named_type() == name)
    };
    let in_field = |field: &ast::FieldDefinition| {
        field.ty.inner_named_type() == name || in_arguments(&field.arguments)
    };
    schema
        .directive_definitions
        .values()
        .any(|d| in_arguments(&d.arguments))
        || schema.types.values().any(|ty| match ty {
            ExtendedType::Object(object) => {
                object.implements_interfaces.contains(name)
                    || object.fields.values().any(|f| in_field(f))
            }
            ExtendedType::Interface(interface) => {
                interface.implements_interfaces.contains(name)
                    || interface.fields.values().any(|f| in_field(f))
            }
            ExtendedType::Union(union_) => union_.members.contains(name),
            ExtendedType::InputObject(input) => input
                .fields
                .values()
                .any(|f| f.ty.inner_named_type() == name),
            _ => false,
        })
}
