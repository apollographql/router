//! Extraction of GraphQL Federation (composite schemas) subgraphs from a supergraph.
//!
//! Composition records source-schema metadata in join v0.6 (see
//! [`crate::composite_schemas::internal`] and the merger): `@join__field(lookup:, lookupArguments:,
//! requireArguments:)` and `@join__graph(internalDefinitions:)`. Extraction turns it back into
//! `@lookup`, `@is` and `@require` applications on the extracted subgraph, and restores the
//! `@internal` elements, so that the query planner reads lookups from the extracted subgraph
//! exactly as it reads `@key`.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::ObjectType;

use super::subgraph::FederationSubgraph;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_FIELD_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::join_spec_definition::FieldDirectiveArguments;
use crate::link::join_spec_definition::JoinArgument;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;

/// The graphs (by `join__Graph` value) that are GraphQL Federation source schemas, with their
/// internal definitions, if any.
pub(super) type CompositeGraphs = IndexMap<Name, Option<String>>;

/// Find the source-schema graphs of a supergraph: those with internal definitions, or whose
/// fields carry lookup metadata.
pub(super) fn composite_graphs(
    supergraph_schema: &FederationSchema,
    join_spec_definition: &JoinSpecDefinition,
) -> Result<CompositeGraphs, FederationError> {
    let mut graphs = CompositeGraphs::default();
    if !join_spec_definition.supports_composite_schemas() {
        return Ok(graphs);
    }
    let graph_directive = join_spec_definition.graph_directive_definition(supergraph_schema)?;
    for (value_name, value) in join_spec_definition
        .graph_enum_definition(supergraph_schema)?
        .values
        .iter()
    {
        if let Some(application) = value.directives.get(&graph_directive.name)
            && let Some(sdl) = join_spec_definition
                .graph_directive_arguments(application)?
                .internal_definitions
        {
            graphs.insert(value_name.clone(), Some(sdl.to_string()));
        }
    }
    let field_directive = &join_spec_definition
        .field_directive_definition(supergraph_schema)?
        .name;
    for ty in supergraph_schema.schema().types.values() {
        let fields: Vec<&Node<FieldDefinition>> = match ty {
            ExtendedType::Object(object) => object.fields.values().collect(),
            ExtendedType::Interface(interface) => interface.fields.values().collect(),
            _ => continue,
        };
        for field in fields {
            for application in field.directives.get_all(field_directive) {
                let arguments = join_spec_definition.field_directive_arguments(application)?;
                if (arguments.lookup || !arguments.require_arguments.is_empty())
                    && let Some(graph) = arguments.graph
                {
                    graphs.entry(graph).or_insert(None);
                }
            }
        }
    }
    Ok(graphs)
}

fn directive_name(
    federation_spec_definition: &FederationSpecDefinition,
    schema: &FederationSchema,
    name_in_spec: &Name,
) -> Result<Name, FederationError> {
    federation_spec_definition
        .directive_name_in_schema(schema, name_in_spec)
        .ok_or_else(|| {
            SingleFederationError::InvalidFederationSupergraph {
                message: format!("extracted subgraph does not define @{name_in_spec}"),
            }
            .into()
        })
}

fn selection_map_directive(name: Name, selection: &str) -> Node<ast::Directive> {
    Node::new(ast::Directive {
        name,
        arguments: vec![Node::new(ast::Argument {
            name: FEDERATION_FIELD_ARGUMENT_NAME,
            value: Node::new(ast::Value::String(selection.to_string())),
        })],
    })
}

/// Apply a field's lookup metadata to the extracted field: `@lookup`, the lookup's arguments with
/// `@is` on the mapped ones, and the `@require` arguments the merged schema does not have.
pub(super) fn apply_field_metadata(
    subgraph_field: &mut FieldDefinition,
    application: &FieldDirectiveArguments,
    schema: &FederationSchema,
    federation_spec_definition: &FederationSpecDefinition,
) -> Result<(), FederationError> {
    if application.lookup {
        subgraph_field.directives.push(Node::new(ast::Directive {
            name: directive_name(
                federation_spec_definition,
                schema,
                &FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC,
            )?,
            arguments: Vec::new(),
        }));
    }
    if !application.lookup_arguments.is_empty() {
        let is = directive_name(
            federation_spec_definition,
            schema,
            &FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC,
        )?;
        // Rebuilt in the graph's order, as the merged field may lack some of them.
        let mut merged = std::mem::take(&mut subgraph_field.arguments);
        for recorded in &application.lookup_arguments {
            let mut argument = match merged.iter().position(|a| a.name == recorded.name) {
                Some(index) => merged.remove(index),
                None => recorded_argument(recorded)?,
            };
            if let Some(selection) = recorded.selection {
                argument
                    .make_mut()
                    .directives
                    .push(selection_map_directive(is.clone(), selection));
            }
            subgraph_field.arguments.push(argument);
        }
        subgraph_field.arguments.extend(merged);
    }
    if !application.require_arguments.is_empty() {
        let require = directive_name(
            federation_spec_definition,
            schema,
            &FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC,
        )?;
        for recorded in &application.require_arguments {
            let Some(selection) = recorded.selection else {
                return Err(SingleFederationError::InvalidFederationSupergraph {
                    message: format!(
                        "@join__field(requireArguments:) has no selection for argument \"{}\"",
                        recorded.name
                    ),
                }
                .into());
            };
            let mut argument = recorded_argument(recorded)?;
            argument
                .make_mut()
                .directives
                .push(selection_map_directive(require.clone(), selection));
            subgraph_field.arguments.push(argument);
        }
    }
    Ok(())
}

/// The definition of an argument recorded in `@join__field`, without directives.
fn recorded_argument(
    recorded: &JoinArgument,
) -> Result<Node<InputValueDefinition>, FederationError> {
    let invalid = |what: &str| -> FederationError {
        SingleFederationError::InvalidFederationSupergraph {
            message: format!(
                "invalid {what} for argument \"{}\" in @join__field",
                recorded.name
            ),
        }
        .into()
    };
    // Parsed as an input field, since apollo-compiler has no parser for a lone value.
    let sdl = match recorded.default {
        Some(default) => format!("input T {{ f: {} = {default} }}", recorded.type_),
        None => format!("input T {{ f: {} }}", recorded.type_),
    };
    let document = ast::Document::parse(sdl, "argument.graphql")
        .map_err(|_| invalid("type or default value"))?;
    let Some(ast::Definition::InputObjectTypeDefinition(input)) = document.definitions.first()
    else {
        return Err(invalid("type or default value"));
    };
    let [field] = input.fields.as_slice() else {
        return Err(invalid("type or default value"));
    };
    Ok(Node::new(InputValueDefinition {
        description: None,
        name: Name::new(recorded.name).map_err(|_| invalid("name"))?,
        ty: field.ty.clone(),
        default_value: field.default_value.clone(),
        directives: Default::default(),
    }))
}

/// Rename the specification-named directives of an internal-definitions field to their names in
/// the extracted subgraph.
fn rename_directives(field: &FieldDefinition, renames: &[(Name, Name)]) -> Node<FieldDefinition> {
    let rename = |directives: &ast::DirectiveList| {
        ast::DirectiveList(
            directives
                .iter()
                .map(|d| match renames.iter().find(|(spec, _)| *spec == d.name) {
                    Some((_, in_schema)) => Node::new(ast::Directive {
                        name: in_schema.clone(),
                        arguments: d.arguments.clone(),
                    }),
                    None => d.clone(),
                })
                .collect(),
        )
    };
    Node::new(FieldDefinition {
        description: None,
        name: field.name.clone(),
        arguments: field
            .arguments
            .iter()
            .map(|argument| {
                Node::new(InputValueDefinition {
                    description: None,
                    name: argument.name.clone(),
                    ty: argument.ty.clone(),
                    default_value: argument.default_value.clone(),
                    directives: rename(&argument.directives),
                })
            })
            .collect(),
        ty: field.ty.clone(),
        directives: rename(&field.directives),
    })
}

fn parse_internal_definitions(
    subgraph_name: &str,
    sdl: &str,
) -> Result<ast::Document, FederationError> {
    ast::Document::parse(sdl, "internal.graphql").map_err(|e| {
        SingleFederationError::InvalidFederationSupergraph {
            message: format!(
                "invalid @join__graph(internalDefinitions:) for subgraph \"{subgraph_name}\": {}",
                e.errors
            ),
        }
        .into()
    })
}

/// First phase of restoring a graph's `@internal` elements: add the types (internal object and
/// interface types empty, internal input types complete), before any field referencing them is
/// extracted.
pub(super) fn add_internal_types(
    subgraph: &mut FederationSubgraph,
    sdl: &str,
) -> Result<(), FederationError> {
    let document = parse_internal_definitions(&subgraph.name, sdl)?;
    let schema = &mut subgraph.schema;
    for definition in &document.definitions {
        match definition {
            ast::Definition::ObjectTypeDefinition(object) => {
                ensure_object(schema, &object.name)?;
            }
            ast::Definition::InterfaceTypeDefinition(interface) => {
                ensure_interface(schema, &interface.name)?;
            }
            // Extracted normally when another subgraph keeps the type public in the supergraph.
            ast::Definition::InputObjectTypeDefinition(input)
                if !schema.schema().types.contains_key(&input.name)
                    && !schema.referencers().contains_type_name(&input.name) =>
            {
                let position = InputObjectTypeDefinitionPosition {
                    type_name: input.name.clone(),
                };
                position.pre_insert(schema)?;
            }
            _ => {}
        }
    }
    // Input objects are inserted once all of them are pre-inserted, as they may reference each
    // other.
    for definition in &document.definitions {
        if let ast::Definition::InputObjectTypeDefinition(input) = definition
            && !schema.schema().types.contains_key(&input.name)
        {
            InputObjectTypeDefinitionPosition {
                type_name: input.name.clone(),
            }
            .insert(
                schema,
                Node::new(InputObjectType {
                    description: None,
                    name: input.name.clone(),
                    directives: input.directives.iter().cloned().collect(),
                    fields: input
                        .fields
                        .iter()
                        .map(|field| (field.name.clone(), Node::new((**field).clone())))
                        .collect(),
                }),
            )?;
        }
    }
    Ok(())
}

fn ensure_interface(schema: &mut FederationSchema, name: &Name) -> Result<(), FederationError> {
    if schema.schema().types.contains_key(name) {
        return Ok(());
    }
    let position = InterfaceTypeDefinitionPosition {
        type_name: name.clone(),
    };
    position.pre_insert(schema)?;
    position.insert(
        schema,
        Node::new(InterfaceType {
            description: None,
            name: name.clone(),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        }),
    )
}

fn ensure_object(schema: &mut FederationSchema, name: &Name) -> Result<(), FederationError> {
    if schema.schema().types.contains_key(name) {
        return Ok(());
    }
    let position = ObjectTypeDefinitionPosition::new(name.clone());
    position.pre_insert(schema)?;
    position.insert(
        schema,
        Node::new(ObjectType {
            description: None,
            name: name.clone(),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        }),
    )
}

/// Second phase of restoring a graph's `@internal` elements: add the internal fields (on internal
/// types and on public types).
pub(super) fn add_internal_definitions(
    subgraph: &mut FederationSubgraph,
    sdl: &str,
    federation_spec_definition: &FederationSpecDefinition,
) -> Result<(), FederationError> {
    let document = parse_internal_definitions(&subgraph.name, sdl)?;
    let schema = &mut subgraph.schema;
    let renames: Vec<(Name, Name)> = [
        FEDERATION_LOOKUP_DIRECTIVE_NAME_IN_SPEC,
        FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC,
        FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC,
    ]
    .into_iter()
    .map(|spec| directive_name(federation_spec_definition, schema, &spec).map(|name| (spec, name)))
    .collect::<Result<_, _>>()?;

    for definition in &document.definitions {
        let (type_name, fields, is_object) = match definition {
            ast::Definition::ObjectTypeDefinition(object) => (&object.name, &object.fields, true),
            ast::Definition::ObjectTypeExtension(object) => (&object.name, &object.fields, true),
            ast::Definition::InterfaceTypeDefinition(interface) => {
                (&interface.name, &interface.fields, false)
            }
            ast::Definition::InterfaceTypeExtension(interface) => {
                (&interface.name, &interface.fields, false)
            }
            // Added by `add_internal_types`.
            ast::Definition::InputObjectTypeDefinition(_) => continue,
            _ => {
                return Err(SingleFederationError::InvalidFederationSupergraph {
                    message: format!(
                        "unexpected definition in @join__graph(internalDefinitions:) for subgraph \
                         \"{}\"",
                        subgraph.name
                    ),
                }
                .into());
            }
        };
        if is_object {
            ensure_object(schema, type_name)?;
        } else {
            ensure_interface(schema, type_name)?;
        }
        for field in fields {
            let field_name = field.name.clone();
            let field = rename_directives(field, &renames);
            if is_object {
                ObjectFieldDefinitionPosition {
                    type_name: type_name.clone(),
                    field_name,
                }
                .insert(schema, field)?;
            } else {
                InterfaceFieldDefinitionPosition {
                    type_name: type_name.clone(),
                    field_name,
                }
                .insert(schema, field)?;
            }
        }
    }
    Ok(())
}
