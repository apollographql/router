//! Normalization of validated source schemas into the shape the merger expects.
//!
//! After validation (which needs the schema exactly as written), each GraphQL Federation source
//! schema is rewritten so that the rest of composition can treat it as an ordinary federation
//! subgraph:
//!
//! - **`@key` resolvability is derived from lookups.** A key some `@lookup` field recalls the
//!   entity by is resolvable; any other declared key is identity only and gets
//!   `resolvable: false`. A key that is only implied by a lookup's arguments is added
//!   (the specification lets it be omitted).
//! - **`@internal` elements are removed.** They do not participate in merging and may collide
//!   freely across source schemas. They are kept as an SDL fragment that the supergraph carries on
//!   the graph's `@join__graph(internalDefinitions:)`, so that extraction can put internal lookup
//!   fields back for query planning.
//!
//! `@lookup`, `@is` and `@require` stay on the (public) schema; the merger records them in join
//! metadata.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;

use super::CompositeNames;
use super::lookups::collect_lookups;
use crate::composition::CompositionFailure;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;
use crate::schema::ValidFederationSchema;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_selection_map::value::SelectionTree;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

/// Normalize every GraphQL Federation source schema among `subgraphs`; federation subgraphs pass
/// through unchanged.
pub(crate) fn normalize_source_schemas(
    subgraphs: Vec<Subgraph<Validated>>,
) -> Result<Vec<Subgraph<Validated>>, CompositionFailure> {
    let mut errors = Vec::new();
    let mut normalized = Vec::with_capacity(subgraphs.len());
    for subgraph in subgraphs {
        if !subgraph.metadata().is_composite_schema() {
            normalized.push(subgraph);
            continue;
        }
        let name = subgraph.name.clone();
        match normalize(subgraph) {
            Ok(subgraph) => normalized.push(subgraph),
            Err(error) => errors.extend(error.into_errors().into_iter().map(|error| {
                CompositionError::SubgraphError {
                    subgraph: name.clone(),
                    error,
                    locations: Vec::new(),
                }
            })),
        }
    }
    if errors.is_empty() {
        Ok(normalized)
    } else {
        Err(CompositionFailure::from_errors(errors))
    }
}

/// Canonical form of a key field set on `type_name`, for comparing keys written differently.
fn canonical_key(schema: &Valid<Schema>, type_name: &Name, fields: &str) -> Option<String> {
    let field_set =
        FieldSet::parse_and_validate(schema, type_name.clone(), fields, "key.graphql").ok()?;
    Some(
        SelectionTree::from_selection_set(&field_set.selection_set)
            .canonical()
            .to_string(),
    )
}

fn normalize(subgraph: Subgraph<Validated>) -> Result<Subgraph<Validated>, FederationError> {
    let names = CompositeNames::new(
        subgraph.schema(),
        subgraph.metadata().federation_spec_definition(),
    )
    .ok_or_else(|| internal_error!("source schema without composite directive names"))?;
    let valid_schema = subgraph.validated_schema().schema().clone();

    // Keys recalled by some lookup, per concrete type (canonical form), including lookups that are
    // internal: they are removed from the merged schema, but still resolve entities.
    let mut lookup_keys: IndexMap<Name, IndexSet<String>> = IndexMap::default();
    for lookup in collect_lookups(&valid_schema, &names.lookup, &names.is) {
        for key in lookup.keys(&valid_schema) {
            lookup_keys
                .entry(key.concrete_type)
                .or_default()
                .insert(key.key.to_string());
        }
    }

    let mut schema: Schema = valid_schema.clone().into_inner();
    let mut errors = Vec::new();
    for (type_name, ty) in &valid_schema.types {
        let recalled: IndexSet<String> = match ty {
            ExtendedType::Object(_) => lookup_keys.get(type_name).cloned().unwrap_or_default(),
            // An interface key is resolvable when every implementation is recalled by it.
            ExtendedType::Interface(_) => {
                let mut implementations = possible_types(&valid_schema, type_name).into_iter();
                match implementations.next() {
                    Some(first) => {
                        let mut common = lookup_keys.get(&first).cloned().unwrap_or_default();
                        for implementation in implementations {
                            let keys = lookup_keys.get(&implementation);
                            common.retain(|key| keys.is_some_and(|keys| keys.contains(key)));
                        }
                        common
                    }
                    None => IndexSet::default(),
                }
            }
            _ => continue,
        };

        let mut declared = IndexSet::default();
        let mut not_recalled = Vec::new();
        for (index, directive) in ty.directives().iter().enumerate() {
            if directive.name != names.key {
                continue;
            }
            let Some(fields) = directive
                .specified_argument_by_name("fields")
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let canonical = canonical_key(&valid_schema, type_name, fields)
                .unwrap_or_else(|| fields.to_string());
            if !recalled.contains(&canonical) {
                not_recalled.push(index);
            }
            declared.insert(canonical);
        }
        let to_add: Vec<&String> = match ty {
            ExtendedType::Object(_) => recalled.iter().filter(|k| !declared.contains(*k)).collect(),
            _ => Vec::new(),
        };
        for key in &to_add {
            if canonical_key(&valid_schema, type_name, key).is_none() {
                errors.push(SingleFederationError::KeyInvalidFields {
                    target_type: type_name.clone(),
                    application: format!("@key(fields: \"{key}\")"),
                    message: format!(
                        "the key implied by a @lookup field is not a valid field set on \
                         \"{type_name}\" in this source schema; declare the key fields on \
                         \"{type_name}\" (as @external if another source schema resolves them)"
                    ),
                });
            }
        }
        if not_recalled.is_empty() && to_add.is_empty() {
            continue;
        }
        let directives = match schema.types.get_mut(type_name) {
            Some(ExtendedType::Object(object)) => &mut object.make_mut().directives,
            Some(ExtendedType::Interface(interface)) => &mut interface.make_mut().directives,
            _ => continue,
        };
        for index in not_recalled {
            directives[index]
                .make_mut()
                .arguments
                .push(Node::new(ast::Argument {
                    name: name!("resolvable"),
                    value: Node::new(ast::Value::Boolean(false)),
                }));
        }
        for key in to_add {
            directives.push(Component::new(ast::Directive {
                name: names.key.clone(),
                arguments: vec![Node::new(ast::Argument {
                    name: name!("fields"),
                    value: Node::new(ast::Value::String(key.clone())),
                })],
            }));
        }
    }
    if !errors.is_empty() {
        return Err(crate::error::MultipleFederationErrors { errors }.into());
    }

    let internal_definitions = remove_internal_elements(&mut schema, &names);

    let schema = schema.validate().map_err(|errors| {
        internal_error!(
            "normalizing the source schema produced an invalid schema: {}",
            errors.errors
        )
    })?;
    let schema = ValidFederationSchema::new(schema)?;
    subgraph.with_normalized_schema(schema, internal_definitions)
}

/// Copy a field for the internal-definitions fragment, keeping only the directives the planner
/// reads (`@lookup` on the field, `@is`/`@require` on arguments), under their specification names.
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

/// Input object types reachable only from internal fields or `@require` arguments. They are
/// removed with the internal elements: the specification removes an input type "only used to
/// express requirements" from the composite schema, and an input type only used by internal
/// fields must not leak into it either.
fn private_input_types(
    schema: &Schema,
    names: &CompositeNames,
    internal_types: &[Name],
) -> IndexSet<Name> {
    fn close_over(schema: &Schema, roots: Vec<Name>) -> IndexSet<Name> {
        let mut seen = IndexSet::default();
        let mut stack = roots;
        while let Some(name) = stack.pop() {
            let Some(ExtendedType::InputObject(input)) = schema.types.get(&name) else {
                continue;
            };
            if !seen.insert(name) {
                continue;
            }
            for field in input.fields.values() {
                stack.push(field.ty.inner_named_type().clone());
            }
        }
        seen
    }
    let mut public_roots = Vec::new();
    let mut private_roots = Vec::new();
    for (type_name, ty) in &schema.types {
        let fields: Vec<&ast::FieldDefinition> = match ty {
            ExtendedType::Object(object) => object.fields.values().map(|f| &***f).collect(),
            ExtendedType::Interface(interface) => {
                interface.fields.values().map(|f| &***f).collect()
            }
            _ => continue,
        };
        let type_is_internal = internal_types.contains(type_name);
        for field in fields {
            let field_is_internal = type_is_internal || field.directives.has(&names.internal);
            for argument in &field.arguments {
                let target = argument.ty.inner_named_type().clone();
                if field_is_internal || argument.directives.has(&names.require) {
                    private_roots.push(target);
                } else {
                    public_roots.push(target);
                }
            }
        }
    }
    let public = close_over(schema, public_roots);
    close_over(schema, private_roots)
        .into_iter()
        .filter(|name| !public.contains(name))
        .collect()
}

/// Remove `@internal` types and fields from `schema`, returning them as an SDL fragment (or `None`
/// when there are none). The fragment also carries the input types only internal fields or
/// `@require` arguments use.
fn remove_internal_elements(schema: &mut Schema, names: &CompositeNames) -> Option<String> {
    let mut document = ast::Document::new();
    let internal_types: Vec<Name> = schema
        .types
        .iter()
        .filter(|(_, ty)| {
            matches!(ty, ExtendedType::Object(_)) && ty.directives().has(&names.internal)
        })
        .map(|(name, _)| name.clone())
        .collect();
    let private_inputs = private_input_types(schema, names, &internal_types);
    for input_name in &private_inputs {
        let Some(ExtendedType::InputObject(input)) = schema.types.get(input_name) else {
            continue;
        };
        document
            .definitions
            .push(ast::Definition::InputObjectTypeDefinition(Node::new(
                ast::InputObjectTypeDefinition {
                    description: None,
                    name: input_name.clone(),
                    directives: ast::DirectiveList::new(),
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

    for type_name in &internal_types {
        let Some(ExtendedType::Object(object)) = schema.types.get(type_name) else {
            continue;
        };
        document
            .definitions
            .push(ast::Definition::ObjectTypeDefinition(Node::new(
                ast::ObjectTypeDefinition {
                    description: None,
                    name: type_name.clone(),
                    implements_interfaces: Vec::new(),
                    directives: ast::DirectiveList::new(),
                    fields: object
                        .fields
                        .values()
                        .map(|field| internal_field(field, names))
                        .collect(),
                },
            )));
    }

    for (type_name, ty) in schema.types.iter_mut() {
        if internal_types.contains(type_name) {
            continue;
        }
        match ty {
            ExtendedType::Object(object) => {
                let internal: Vec<Name> = object
                    .fields
                    .iter()
                    .filter(|(_, f)| f.directives.has(&names.internal))
                    .map(|(name, _)| name.clone())
                    .collect();
                if internal.is_empty() {
                    continue;
                }
                let fields = internal
                    .iter()
                    .filter_map(|name| object.fields.get(name))
                    .map(|field| internal_field(field, names))
                    .collect();
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
                let object = object.make_mut();
                for name in internal {
                    object.fields.shift_remove(&name);
                }
            }
            ExtendedType::Interface(interface) => {
                let internal: Vec<Name> = interface
                    .fields
                    .iter()
                    .filter(|(_, f)| f.directives.has(&names.internal))
                    .map(|(name, _)| name.clone())
                    .collect();
                if internal.is_empty() {
                    continue;
                }
                let fields = internal
                    .iter()
                    .filter_map(|name| interface.fields.get(name))
                    .map(|field| internal_field(field, names))
                    .collect();
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
                let interface = interface.make_mut();
                for name in internal {
                    interface.fields.shift_remove(&name);
                }
            }
            ExtendedType::Union(union_)
                if union_
                    .members
                    .iter()
                    .any(|m| internal_types.contains(&m.name)) =>
            {
                union_
                    .make_mut()
                    .members
                    .retain(|m| !internal_types.contains(&m.name));
            }
            _ => {}
        }
    }
    // Private input types stay in the schema: the `@require` arguments the merger reads still
    // reference them. The merger drops them from the supergraph when no subgraph uses them
    // publicly (see `Merger::remove_private_input_types`).
    for type_name in &internal_types {
        schema.types.shift_remove(type_name);
    }

    (!document.definitions.is_empty()).then(|| document.to_string())
}

/// The input types a normalized source schema recorded as private in its internal definitions.
pub(crate) fn private_input_type_names(internal_definitions: &str) -> IndexSet<Name> {
    ast::Document::parse(internal_definitions, "internal.graphql")
        .map(|document| {
            document
                .definitions
                .iter()
                .filter_map(|definition| match definition {
                    ast::Definition::InputObjectTypeDefinition(input) => Some(input.name.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
