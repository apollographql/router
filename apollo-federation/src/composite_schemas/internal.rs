//! Removing `@internal` elements and `@require` arguments before merging.
//!
//! An internal field or type is local to its source schema: it is never part of the composite
//! schema, so it does not take part in merging (and cannot conflict with a public field or type of
//! the same name in another source schema, spec `INVALID_FIELD_SHARING` and "Merge Input Types").
//! Arguments annotated `@require` are supplied by the executor and "do not participate in the
//! merge process" either. Validation, including the cross-schema `@is`/`@require` rules, runs on
//! the unmodified subgraphs before this.
//!
//! The executor still needs both (spec: "Internal lookup fields remain usable for transitions"),
//! so what is removed is returned as [`RemovedElements`]. The merger records it in join v0.6
//! (`@join__graph(internalDefinitions:)` and `@join__field(requireArguments:)`), and extraction
//! puts it back.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;

use super::CompositeNames;
use super::validation::fields_of;
use super::validation::is_federation_root_field;
use crate::link::federation_spec_definition::FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;

/// The elements [`remove_internal_elements`] took out of a source schema.
#[derive(Debug, Clone, Default)]
pub(crate) struct RemovedElements {
    /// SDL of the internal types, fields and input types. Only the directives the planner reads
    /// are kept (`@lookup`, `@is`, `@require`), under their specification names.
    pub(crate) internal_definitions: Option<String>,
    /// The `@require` arguments removed from public fields, by parent type and field name, with
    /// directive names as in the source schema.
    pub(crate) require_arguments: IndexMap<(Name, Name), Vec<Node<ast::InputValueDefinition>>>,
}

/// Remove from a subgraph schema (directive names as in `schema`):
/// - fields annotated `@internal`;
/// - object and interface types annotated `@internal`, or whose fields are all `@internal`, other
///   than root operation types (only their internal fields are removed, which is every field
///   but `_service`/`_entities` for a root type annotated `@internal`);
/// - arguments annotated `@require` on the remaining fields;
/// - input object types annotated `@internal`. Source-schema validation
///   (`INTERNAL_INPUT_TYPE_REFERENCED_PUBLICLY`) guarantees that only the elements removed above
///   reference them.
///
/// Returns what was removed, or `None` when nothing was.
pub(crate) fn remove_internal_elements(
    schema: &mut Schema,
    names: &CompositeNames,
) -> Option<RemovedElements> {
    let internal = &names.internal;
    let require = &names.require;
    let root_types: IndexSet<Name> = schema
        .schema_definition
        .iter_root_operations()
        .map(|(_, name)| Name::clone(name))
        .collect();
    let internal_types: IndexSet<Name> = schema
        .types
        .iter()
        .filter(|(name, ty)| {
            !root_types.contains(*name)
                && (matches!(ty, ExtendedType::Object(_) | ExtendedType::Interface(_))
                    && (ty.directives().has(internal)
                        || fields_of(ty).is_some_and(|fields| {
                            fields.values().all(|f| f.directives.has(internal))
                        })))
        })
        .map(|(name, _)| name.clone())
        .collect();
    let mut document = ast::Document::new();
    let mut removed = RemovedElements::default();
    let mut changed = false;

    for name in &internal_types {
        let Some(ty) = schema.types.shift_remove(name) else {
            continue;
        };
        changed = true;
        match ty {
            ExtendedType::Object(object) => {
                document
                    .definitions
                    .push(ast::Definition::ObjectTypeDefinition(Node::new(
                        ast::ObjectTypeDefinition {
                            description: None,
                            name: name.clone(),
                            implements_interfaces: Vec::new(),
                            directives: ast::DirectiveList::new(),
                            fields: object
                                .fields
                                .values()
                                .map(|field| internal_field(field, names))
                                .collect(),
                        },
                    )))
            }
            ExtendedType::Interface(interface) => {
                document
                    .definitions
                    .push(ast::Definition::InterfaceTypeDefinition(Node::new(
                        ast::InterfaceTypeDefinition {
                            description: None,
                            name: name.clone(),
                            implements_interfaces: Vec::new(),
                            directives: ast::DirectiveList::new(),
                            fields: interface
                                .fields
                                .values()
                                .map(|field| internal_field(field, names))
                                .collect(),
                        },
                    )))
            }
            _ => {}
        }
    }

    for (type_name, ty) in schema.types.iter_mut() {
        match ty {
            ExtendedType::Object(object) => {
                // Only a root type can still be annotated `@internal` here: the annotation is
                // shorthand for `@internal` on each of its fields, but the type itself stays.
                let type_is_internal = object.directives.has(internal);
                let is_internal = |name: &Name, f: &ast::FieldDefinition| {
                    f.directives.has(internal)
                        || (type_is_internal && !is_federation_root_field(name))
                };
                let fields: Vec<_> = object
                    .fields
                    .iter()
                    .filter(|(name, f)| is_internal(name, f))
                    .map(|(_, f)| internal_field(f, names))
                    .collect();
                let object = object.make_mut();
                if !fields.is_empty() {
                    object.fields.retain(|name, f| !is_internal(name, f));
                    document
                        .definitions
                        .push(ast::Definition::ObjectTypeExtension(Node::new(
                            ast::ObjectTypeExtension {
                                name: type_name.clone(),
                                implements_interfaces: Vec::new(),
                                directives: ast::DirectiveList::new(),
                                fields,
                            },
                        )));
                    changed = true;
                }
                for field in object.fields.values_mut() {
                    changed |= take_require_arguments(type_name, field, require, &mut removed);
                }
                let before = object.implements_interfaces.len();
                object
                    .implements_interfaces
                    .retain(|i| !internal_types.contains(&**i));
                changed |= before != object.implements_interfaces.len();
            }
            ExtendedType::Interface(interface) => {
                let fields: Vec<_> = interface
                    .fields
                    .values()
                    .filter(|f| f.directives.has(internal))
                    .map(|f| internal_field(f, names))
                    .collect();
                let interface = interface.make_mut();
                if !fields.is_empty() {
                    interface.fields.retain(|_, f| !f.directives.has(internal));
                    document
                        .definitions
                        .push(ast::Definition::InterfaceTypeExtension(Node::new(
                            ast::InterfaceTypeExtension {
                                name: type_name.clone(),
                                implements_interfaces: Vec::new(),
                                directives: ast::DirectiveList::new(),
                                fields,
                            },
                        )));
                    changed = true;
                }
                for field in interface.fields.values_mut() {
                    changed |= take_require_arguments(type_name, field, require, &mut removed);
                }
                let before = interface.implements_interfaces.len();
                interface
                    .implements_interfaces
                    .retain(|i| !internal_types.contains(&**i));
                changed |= before != interface.implements_interfaces.len();
            }
            _ => {}
        }
    }

    let internal_inputs: Vec<Name> = schema
        .types
        .iter()
        .filter(|(_, ty)| {
            matches!(ty, ExtendedType::InputObject(_)) && ty.directives().has(internal)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in internal_inputs {
        let Some(ExtendedType::InputObject(input)) = schema.types.shift_remove(&name) else {
            continue;
        };
        changed = true;
        document
            .definitions
            .push(ast::Definition::InputObjectTypeDefinition(Node::new(
                ast::InputObjectTypeDefinition {
                    description: None,
                    name,
                    // `@oneOf` decides whether an `@is` map with alternatives is valid.
                    directives: input.directives.get("oneOf").cloned().into_iter().collect(),
                    fields: input
                        .fields
                        .values()
                        .map(|field| {
                            Node::new(ast::InputValueDefinition {
                                description: None,
                                name: field.name.clone(),
                                ty: field.ty.clone(),
                                default_value: field.default_value.clone(),
                                directives: ast::DirectiveList::new(),
                            })
                        })
                        .collect(),
                },
            )));
    }

    if !changed {
        return None;
    }
    removed.internal_definitions = (!document.definitions.is_empty()).then(|| document.to_string());
    Some(removed)
}

/// Move the `@require` arguments of `field` into `removed`. Returns whether there were any.
fn take_require_arguments(
    type_name: &Name,
    field: &mut Node<ast::FieldDefinition>,
    require: &Name,
    removed: &mut RemovedElements,
) -> bool {
    if !field.arguments.iter().any(|a| a.directives.has(require)) {
        return false;
    }
    let field = field.make_mut();
    let (required, kept) = std::mem::take(&mut field.arguments)
        .into_iter()
        .partition(|a| a.directives.has(require));
    field.arguments = kept;
    removed
        .require_arguments
        .insert((type_name.clone(), field.name.clone()), required);
    true
}

/// Copy a field for the internal definitions, keeping only the directives the planner reads
/// (`@lookup` on the field, `@is`/`@require` on arguments), under their specification names, so
/// that extraction can rename them for the extracted subgraph.
fn internal_field(
    field: &ast::FieldDefinition,
    names: &CompositeNames,
) -> Node<ast::FieldDefinition> {
    let keep = |directives: &ast::DirectiveList, pairs: &[(&Name, Name)]| {
        ast::DirectiveList(
            directives
                .iter()
                .filter_map(|d| {
                    pairs.iter().find(|(n, _)| **n == d.name).map(|(_, spec)| {
                        Node::new(ast::Directive {
                            name: spec.clone(),
                            arguments: d.arguments.clone(),
                        })
                    })
                })
                .collect(),
        )
    };
    Node::new(ast::FieldDefinition {
        description: None,
        name: field.name.clone(),
        arguments: field
            .arguments
            .iter()
            .map(|argument| {
                Node::new(ast::InputValueDefinition {
                    description: None,
                    name: argument.name.clone(),
                    ty: argument.ty.clone(),
                    default_value: argument.default_value.clone(),
                    directives: keep(
                        &argument.directives,
                        &[
                            (&names.is, FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC),
                            (&names.require, FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC),
                        ],
                    ),
                })
            })
            .collect(),
        ty: field.ty.clone(),
        directives: keep(
            &field.directives,
            &[(&names.lookup, FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC)],
        ),
    })
}

/// A subgraph applying the GraphQL Federation directives, after normalization: the names it uses
/// for them, and what was removed before merging.
#[derive(Debug, Clone)]
pub(crate) struct NormalizedSourceSchema {
    pub(crate) names: CompositeNames,
    pub(crate) removed: RemovedElements,
}
