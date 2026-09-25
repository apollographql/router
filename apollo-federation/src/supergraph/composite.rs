//! Extraction of GraphQL Federation (composite schemas) subgraphs from a supergraph.
//!
//! Composition records source-schema metadata in join v0.6 (see
//! [`crate::composite_schemas::normalize`] and the merger): `@join__field(lookup:, isArguments:,
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
use apollo_compiler::schema::Component;
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
        let fields: Vec<&Component<FieldDefinition>> = match ty {
            ExtendedType::Object(object) => object.fields.values().collect(),
            ExtendedType::Interface(interface) => interface.fields.values().collect(),
            _ => continue,
        };
        for field in fields {
            for application in field.directives.get_all(field_directive) {
                let arguments = join_spec_definition.field_directive_arguments(application)?;
                if (arguments.lookup
                    || !arguments.is_arguments.is_empty()
                    || !arguments.require_arguments.is_empty())
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

/// Apply a field's lookup metadata to the extracted field: `@lookup`, `@is` on the mapped
/// arguments, and the `@require` arguments the merged schema does not have.
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
    if !application.is_arguments.is_empty() {
        let is = directive_name(
            federation_spec_definition,
            schema,
            &FEDERATION_IS_DIRECTIVE_NAME_IN_SPEC,
        )?;
        for is_argument in &application.is_arguments {
            let Some(argument) = subgraph_field
                .arguments
                .iter_mut()
                .find(|a| a.name == is_argument.name)
            else {
                return Err(SingleFederationError::InvalidFederationSupergraph {
                    message: format!(
                        "@join__field(isArguments:) names argument \"{}\", which field \"{}\" \
                         does not have",
                        is_argument.name, subgraph_field.name
                    ),
                }
                .into());
            };
            argument
                .make_mut()
                .directives
                .push(selection_map_directive(is.clone(), is_argument.selection));
        }
    }
    if !application.require_arguments.is_empty() {
        let require = directive_name(
            federation_spec_definition,
            schema,
            &FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC,
        )?;
        for require_argument in &application.require_arguments {
            let ty = ast::Type::parse(require_argument.type_, "require.graphql").map_err(|e| {
                SingleFederationError::InvalidFederationSupergraph {
                    message: format!(
                        "invalid type \"{}\" in @join__field(requireArguments:): {e}",
                        require_argument.type_
                    ),
                }
            })?;
            subgraph_field
                .arguments
                .push(Node::new(InputValueDefinition {
                    description: None,
                    name: Name::new(require_argument.name)?,
                    ty: Node::new(ty),
                    default_value: None,
                    directives: ast::DirectiveList(vec![selection_map_directive(
                        require.clone(),
                        require_argument.selection,
                    )]),
                }));
        }
    }
    Ok(())
}

/// Rename the specification-named directives of an internal-definitions field to their names in
/// the extracted subgraph.
fn rename_directives(
    field: &FieldDefinition,
    renames: &[(Name, Name)],
) -> Component<FieldDefinition> {
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
    Component::new(FieldDefinition {
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

/// First phase of restoring a graph's `@internal` elements: add the types (internal object types
/// empty, private input types complete), before any field referencing them is extracted.
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
                    directives: Default::default(),
                    fields: input
                        .fields
                        .iter()
                        .map(|field| (field.name.clone(), Component::new((**field).clone())))
                        .collect(),
                }),
            )?;
        }
    }
    Ok(())
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
        match definition {
            ast::Definition::ObjectTypeDefinition(object) => {
                ensure_object(schema, &object.name)?;
                for field in &object.fields {
                    ObjectFieldDefinitionPosition {
                        type_name: object.name.clone(),
                        field_name: field.name.clone(),
                    }
                    .insert(schema, rename_directives(field, &renames))?;
                }
            }
            ast::Definition::ObjectTypeExtension(extension) => {
                ensure_object(schema, &extension.name)?;
                for field in &extension.fields {
                    ObjectFieldDefinitionPosition {
                        type_name: extension.name.clone(),
                        field_name: field.name.clone(),
                    }
                    .insert(schema, rename_directives(field, &renames))?;
                }
            }
            ast::Definition::InterfaceTypeExtension(extension) => {
                if !schema.schema().types.contains_key(&extension.name) {
                    let position = InterfaceTypeDefinitionPosition {
                        type_name: extension.name.clone(),
                    };
                    position.pre_insert(schema)?;
                    position.insert(
                        schema,
                        Node::new(InterfaceType {
                            description: None,
                            name: extension.name.clone(),
                            implements_interfaces: Default::default(),
                            directives: Default::default(),
                            fields: Default::default(),
                        }),
                    )?;
                }
                for field in &extension.fields {
                    InterfaceFieldDefinitionPosition {
                        type_name: extension.name.clone(),
                        field_name: field.name.clone(),
                    }
                    .insert(schema, rename_directives(field, &renames))?;
                }
            }
            // Added by `add_internal_types`.
            ast::Definition::InputObjectTypeDefinition(_) => {}
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
        }
    }
    Ok(())
}
