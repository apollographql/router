use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write;
use std::ops::Deref;
use std::sync::Arc;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::ComponentOrigin;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::DirectiveLocation;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ExtensionId;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::NamedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::SchemaBuilder;
use apollo_compiler::schema::Type;
use apollo_compiler::schema::UnionType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Node;
use itertools::Itertools;
use lazy_static::lazy_static;
use time::OffsetDateTime;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::cost_spec_definition::CostSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::join_spec_definition::FieldDirectiveArguments;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::join_spec_definition::TypeDirectiveArguments;
use crate::link::spec::Identity;
use crate::link::spec::Version;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::spec_definition::SpecDefinition;
use crate::link::Link;
use crate::link::DEFAULT_LINK_NAME;
use crate::schema::field_set::parse_field_set_without_normalization;
use crate::schema::position::is_graphql_reserved_name;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::SchemaRootDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::type_and_directive_specification::FieldSpecification;
use crate::schema::type_and_directive_specification::ObjectTypeSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;
use crate::schema::type_and_directive_specification::UnionTypeSpecification;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;

/// Assumes the given schema has been validated.
///
/// TODO: A lot of common data gets passed around in the functions called by this one, considering
/// making an e.g. ExtractSubgraphs struct to contain the data.
pub(crate) fn extract_subgraphs_from_supergraph(
    supergraph_schema: &FederationSchema,
    validate_extracted_subgraphs: Option<bool>,
) -> Result<ValidFederationSubgraphs, FederationError> {
    let validate_extracted_subgraphs = validate_extracted_subgraphs.unwrap_or(true);
    let (link_spec_definition, join_spec_definition) =
        crate::validate_supergraph_for_query_planning(supergraph_schema)?;
    let is_fed_1 = *join_spec_definition.version() == Version { major: 0, minor: 1 };
    let (mut subgraphs, federation_spec_definitions, graph_enum_value_name_to_subgraph_name) =
        collect_empty_subgraphs(supergraph_schema, join_spec_definition)?;

    let mut filtered_types = Vec::new();
    for type_definition_position in supergraph_schema.get_types() {
        if !join_spec_definition
            .is_spec_type_name(supergraph_schema, type_definition_position.type_name())?
            && !link_spec_definition
                .is_spec_type_name(supergraph_schema, type_definition_position.type_name())?
        {
            filtered_types.push(type_definition_position);
        }
    }
    if is_fed_1 {
        let unsupported =
            SingleFederationError::UnsupportedFederationVersion {
                message: String::from("Supergraphs composed with federation version 1 are not supported. Please recompose your supergraph with federation version 2 or greater")
            };
        return Err(unsupported.into());
    } else {
        extract_subgraphs_from_fed_2_supergraph(
            supergraph_schema,
            &mut subgraphs,
            &graph_enum_value_name_to_subgraph_name,
            &federation_spec_definitions,
            join_spec_definition,
            &filtered_types,
        )?;
    }

    for graph_enum_value in graph_enum_value_name_to_subgraph_name.keys() {
        let subgraph = get_subgraph(
            &mut subgraphs,
            &graph_enum_value_name_to_subgraph_name,
            graph_enum_value,
        )?;
        let federation_spec_definition = federation_spec_definitions
            .get(graph_enum_value)
            .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                message: "Subgraph unexpectedly does not use federation spec".to_owned(),
            })?;
        add_federation_operations(subgraph, federation_spec_definition)?;
    }

    let mut valid_subgraphs = ValidFederationSubgraphs::new();
    for (_, mut subgraph) in subgraphs {
        let valid_subgraph_schema = if validate_extracted_subgraphs {
            match subgraph.schema.validate_or_return_self() {
                Ok(schema) => schema,
                Err((schema, error)) => {
                    subgraph.schema = schema;
                    if is_fed_1 {
                        let message =
                                String::from("Supergraphs composed with federation version 1 are not supported. Please recompose your supergraph with federation version 2 or greater");
                        return Err(SingleFederationError::UnsupportedFederationVersion {
                            message,
                        }
                        .into());
                    } else {
                        let mut message = format!(
                                    "Unexpected error extracting {} from the supergraph: this is either a bug, or the supergraph has been corrupted.\n\nDetails:\n{error}",
                                    subgraph.name,
                                    );
                        maybe_dump_subgraph_schema(subgraph, &mut message);
                        return Err(
                            SingleFederationError::InvalidFederationSupergraph { message }.into(),
                        );
                    }
                }
            }
        } else {
            subgraph.schema.assume_valid()?
        };
        valid_subgraphs.add(ValidFederationSubgraph {
            name: subgraph.name,
            url: subgraph.url,
            schema: valid_subgraph_schema,
        })?;
    }

    Ok(valid_subgraphs)
}

type CollectEmptySubgraphsOk = (
    FederationSubgraphs,
    IndexMap<Name, &'static FederationSpecDefinition>,
    IndexMap<Name, Arc<str>>,
);
fn collect_empty_subgraphs(
    supergraph_schema: &FederationSchema,
    join_spec_definition: &JoinSpecDefinition,
) -> Result<CollectEmptySubgraphsOk, FederationError> {
    let mut subgraphs = FederationSubgraphs::new();
    let graph_directive_definition =
        join_spec_definition.graph_directive_definition(supergraph_schema)?;
    let graph_enum = join_spec_definition.graph_enum_definition(supergraph_schema)?;
    let mut federation_spec_definitions = IndexMap::default();
    let mut graph_enum_value_name_to_subgraph_name = IndexMap::default();
    for (enum_value_name, enum_value_definition) in graph_enum.values.iter() {
        let graph_application = enum_value_definition
            .directives
            .get(&graph_directive_definition.name)
            .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                message: format!(
                    "Value \"{}\" of join__Graph enum has no @join__graph directive",
                    enum_value_name
                ),
            })?;
        let graph_arguments = join_spec_definition.graph_directive_arguments(graph_application)?;
        let subgraph = FederationSubgraph {
            name: graph_arguments.name.to_owned(),
            url: graph_arguments.url.to_owned(),
            schema: new_empty_fed_2_subgraph_schema()?,
        };
        let federation_link = &subgraph
            .schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&Identity::federation_identity()))
            .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                message: "Subgraph unexpectedly does not use federation spec".to_owned(),
            })?;
        let federation_spec_definition = FEDERATION_VERSIONS
            .find(&federation_link.url.version)
            .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                message: "Subgraph unexpectedly does not use a supported federation spec version"
                    .to_owned(),
            })?;
        subgraphs.add(subgraph)?;
        graph_enum_value_name_to_subgraph_name
            .insert(enum_value_name.clone(), graph_arguments.name.into());
        federation_spec_definitions.insert(enum_value_name.clone(), federation_spec_definition);
    }
    Ok((
        subgraphs,
        federation_spec_definitions,
        graph_enum_value_name_to_subgraph_name,
    ))
}

/// TODO: Use the JS/programmatic approach instead of hard-coding definitions.
pub(crate) fn new_empty_fed_2_subgraph_schema() -> Result<FederationSchema, FederationError> {
    let builder = SchemaBuilder::new().adopt_orphan_extensions();
    let builder = builder.parse(
        r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/federation/v2.9")

    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

    scalar link__Import

    enum link__Purpose {
        """
        \`SECURITY\` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        \`EXECUTION\` features provide metadata necessary for operation execution.
        """
        EXECUTION
    }

    directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

    directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

    directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

    directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

    directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

    directive @federation__extends on OBJECT | INTERFACE

    directive @federation__shareable on OBJECT | FIELD_DEFINITION

    directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

    directive @federation__override(from: String!, label: String) on FIELD_DEFINITION

    directive @federation__composeDirective(name: String) repeatable on SCHEMA

    directive @federation__interfaceObject on OBJECT

    directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

    directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

    directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

    directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

    scalar federation__FieldSet

    scalar federation__Scope
    "#,
        "subgraph.graphql",
    );
    FederationSchema::new(builder.build()?)
}

struct TypeInfo {
    name: NamedType,
    // IndexMap<subgraph_enum_value: String, is_interface_object: bool>
    subgraph_info: IndexMap<Name, bool>,
}

struct TypeInfos {
    object_types: Vec<TypeInfo>,
    interface_types: Vec<TypeInfo>,
    union_types: Vec<TypeInfo>,
    enum_types: Vec<TypeInfo>,
    input_object_types: Vec<TypeInfo>,
}

/// Builds a map of original name to new name for Apollo feature directives. This is
/// used to handle cases where a directive is renamed via an import statement. For
/// example, importing a directive with a custom name like
/// ```graphql
/// @link(url: "https://specs.apollo.dev/cost/v0.1", import: [{ name: "@cost", as: "@renamedCost" }])
/// ```
/// results in a map entry of `cost -> renamedCost` with the `@` prefix removed.
///
/// If the directive is imported under its default name, that also results in an entry. So,
/// ```graphql
/// @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])
/// ```
/// results in a map entry of `cost -> cost`. This duals as a way to check if a directive
/// is included in the supergraph schema.
///
/// **Important:** This map does _not_ include directives imported from identities other
/// than `specs.apollo.dev`. This helps us avoid extracting directives to subgraphs
/// when a custom directive's name conflicts with that of a default one.
fn get_apollo_directive_names(
    supergraph_schema: &FederationSchema,
) -> Result<IndexMap<Name, Name>, FederationError> {
    let mut hm: IndexMap<Name, Name> = IndexMap::default();
    for directive in &supergraph_schema.schema().schema_definition.directives {
        if directive.name.as_str() == "link" {
            if let Ok(link) = Link::from_directive_application(directive) {
                if link.url.identity.domain != APOLLO_SPEC_DOMAIN {
                    continue;
                }
                for import in link.imports {
                    hm.insert(import.element.clone(), import.imported_name().clone());
                }
            }
        }
    }
    Ok(hm)
}

fn extract_subgraphs_from_fed_2_supergraph(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
    join_spec_definition: &'static JoinSpecDefinition,
    filtered_types: &Vec<TypeDefinitionPosition>,
) -> Result<(), FederationError> {
    let original_directive_names = get_apollo_directive_names(supergraph_schema)?;

    let TypeInfos {
        object_types,
        interface_types,
        union_types,
        enum_types,
        input_object_types,
    } = add_all_empty_subgraph_types(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
        federation_spec_definitions,
        join_spec_definition,
        filtered_types,
        &original_directive_names,
    )?;

    extract_object_type_content(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
        federation_spec_definitions,
        join_spec_definition,
        &object_types,
        &original_directive_names,
    )?;
    extract_interface_type_content(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
        federation_spec_definitions,
        join_spec_definition,
        &interface_types,
        &original_directive_names,
    )?;
    extract_union_type_content(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
        join_spec_definition,
        &union_types,
    )?;
    extract_enum_type_content(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
        federation_spec_definitions,
        join_spec_definition,
        &enum_types,
        &original_directive_names,
    )?;
    extract_input_object_type_content(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
        federation_spec_definitions,
        join_spec_definition,
        &input_object_types,
        &original_directive_names,
    )?;

    extract_join_directives(
        supergraph_schema,
        subgraphs,
        graph_enum_value_name_to_subgraph_name,
    )?;

    // We add all the "executable" directive definitions from the supergraph to each subgraphs, as
    // those may be part of a query and end up in any subgraph fetches. We do this "last" to make
    // sure that if one of the directives uses a type for an argument, that argument exists. Note
    // that we don't bother with non-executable directive definitions at the moment since we
    // don't extract their applications. It might become something we need later, but we don't so
    // far. Accordingly, we skip any potentially applied directives in the argument of the copied
    // definition, because we haven't copied type-system directives.
    let all_executable_directive_definitions = supergraph_schema
        .schema()
        .directive_definitions
        .values()
        .filter_map(|directive_definition| {
            let executable_locations = directive_definition
                .locations
                .iter()
                .filter(|location| EXECUTABLE_DIRECTIVE_LOCATIONS.contains(*location))
                .copied()
                .collect::<Vec<_>>();
            if executable_locations.is_empty() {
                return None;
            }
            Some(Node::new(DirectiveDefinition {
                description: None,
                name: directive_definition.name.clone(),
                arguments: directive_definition
                    .arguments
                    .iter()
                    .map(|argument| {
                        Node::new(InputValueDefinition {
                            description: None,
                            name: argument.name.clone(),
                            ty: argument.ty.clone(),
                            default_value: argument.default_value.clone(),
                            directives: Default::default(),
                        })
                    })
                    .collect::<Vec<_>>(),
                repeatable: directive_definition.repeatable,
                locations: executable_locations,
            }))
        })
        .collect::<Vec<_>>();
    for subgraph in subgraphs.subgraphs.values_mut() {
        remove_inactive_requires_and_provides_from_subgraph(
            supergraph_schema,
            &mut subgraph.schema,
        )?;
        remove_unused_types_from_subgraph(&mut subgraph.schema)?;
        for definition in all_executable_directive_definitions.iter() {
            let pos = DirectiveDefinitionPosition {
                directive_name: definition.name.clone(),
            };
            pos.pre_insert(&mut subgraph.schema)?;
            pos.insert(&mut subgraph.schema, definition.clone())?;
        }
    }

    Ok(())
}

fn add_all_empty_subgraph_types(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
    join_spec_definition: &'static JoinSpecDefinition,
    filtered_types: &Vec<TypeDefinitionPosition>,
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<TypeInfos, FederationError> {
    let type_directive_definition =
        join_spec_definition.type_directive_definition(supergraph_schema)?;

    let mut object_types: Vec<TypeInfo> = Vec::new();
    let mut interface_types: Vec<TypeInfo> = Vec::new();
    let mut union_types: Vec<TypeInfo> = Vec::new();
    let mut enum_types: Vec<TypeInfo> = Vec::new();
    let mut input_object_types: Vec<TypeInfo> = Vec::new();

    for type_definition_position in filtered_types {
        let type_ = type_definition_position.get(supergraph_schema.schema())?;
        let mut type_directive_applications = Vec::new();
        for directive in type_.directives().get_all(&type_directive_definition.name) {
            type_directive_applications
                .push(join_spec_definition.type_directive_arguments(directive)?);
        }
        let types_mut = match &type_definition_position {
            TypeDefinitionPosition::Scalar(pos) => {
                // Scalar are a bit special in that they don't have any sub-component, so we don't
                // track them beyond adding them to the proper subgraphs. It's also simple because
                // there is no possible key so there is exactly one @join__type application for each
                // subgraph having the scalar (and most arguments cannot be present).
                for type_directive_application in &type_directive_applications {
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        &type_directive_application.graph,
                    )?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(&type_directive_application.graph)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;

                    pos.pre_insert(&mut subgraph.schema)?;
                    pos.insert(
                        &mut subgraph.schema,
                        Node::new(ScalarType {
                            description: None,
                            name: pos.type_name.clone(),
                            directives: Default::default(),
                        }),
                    )?;

                    if let Some(cost_spec_definition) =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema)
                    {
                        cost_spec_definition.propagate_demand_control_directives_for_scalar(
                            &mut subgraph.schema,
                            pos.get(supergraph_schema.schema())?,
                            pos,
                            original_directive_names,
                        )?;
                    }
                }
                None
            }
            TypeDefinitionPosition::Object(_) => Some(&mut object_types),
            TypeDefinitionPosition::Interface(_) => Some(&mut interface_types),
            TypeDefinitionPosition::Union(_) => Some(&mut union_types),
            TypeDefinitionPosition::Enum(_) => Some(&mut enum_types),
            TypeDefinitionPosition::InputObject(_) => Some(&mut input_object_types),
        };
        if let Some(types_mut) = types_mut {
            types_mut.push(add_empty_type(
                type_definition_position.clone(),
                &type_directive_applications,
                subgraphs,
                graph_enum_value_name_to_subgraph_name,
                federation_spec_definitions,
            )?);
        }
    }

    Ok(TypeInfos {
        object_types,
        interface_types,
        union_types,
        enum_types,
        input_object_types,
    })
}

fn add_empty_type(
    type_definition_position: TypeDefinitionPosition,
    type_directive_applications: &Vec<TypeDirectiveArguments>,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
) -> Result<TypeInfo, FederationError> {
    // In fed2, we always mark all types with `@join__type` but making sure.
    if type_directive_applications.is_empty() {
        return Err(SingleFederationError::InvalidFederationSupergraph {
            message: format!("Missing @join__type on \"{}\"", type_definition_position),
        }
        .into());
    }
    let mut type_info = TypeInfo {
        name: type_definition_position.type_name().clone(),
        subgraph_info: IndexMap::default(),
    };
    for type_directive_application in type_directive_applications {
        let subgraph = get_subgraph(
            subgraphs,
            graph_enum_value_name_to_subgraph_name,
            &type_directive_application.graph,
        )?;
        let federation_spec_definition = federation_spec_definitions
            .get(&type_directive_application.graph)
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!(
                    "Missing federation spec info for subgraph enum value \"{}\"",
                    type_directive_application.graph
                ),
            })?;

        if !type_info
            .subgraph_info
            .contains_key(&type_directive_application.graph)
        {
            let mut is_interface_object = false;
            match &type_definition_position {
                TypeDefinitionPosition::Scalar(_) => {
                    return Err(SingleFederationError::Internal {
                        message: "\"add_empty_type()\" shouldn't be called for scalars".to_owned(),
                    }
                    .into());
                }
                TypeDefinitionPosition::Object(pos) => {
                    pos.pre_insert(&mut subgraph.schema)?;
                    pos.insert(
                        &mut subgraph.schema,
                        Node::new(ObjectType {
                            description: None,
                            name: pos.type_name.clone(),
                            implements_interfaces: Default::default(),
                            directives: Default::default(),
                            fields: Default::default(),
                        }),
                    )?;
                    if pos.type_name == "Query" {
                        let root_pos = SchemaRootDefinitionPosition {
                            root_kind: SchemaRootDefinitionKind::Query,
                        };
                        if root_pos.try_get(subgraph.schema.schema()).is_none() {
                            root_pos.insert(
                                &mut subgraph.schema,
                                ComponentName::from(&pos.type_name),
                            )?;
                        }
                    } else if pos.type_name == "Mutation" {
                        let root_pos = SchemaRootDefinitionPosition {
                            root_kind: SchemaRootDefinitionKind::Mutation,
                        };
                        if root_pos.try_get(subgraph.schema.schema()).is_none() {
                            root_pos.insert(
                                &mut subgraph.schema,
                                ComponentName::from(&pos.type_name),
                            )?;
                        }
                    } else if pos.type_name == "Subscription" {
                        let root_pos = SchemaRootDefinitionPosition {
                            root_kind: SchemaRootDefinitionKind::Subscription,
                        };
                        if root_pos.try_get(subgraph.schema.schema()).is_none() {
                            root_pos.insert(
                                &mut subgraph.schema,
                                ComponentName::from(&pos.type_name),
                            )?;
                        }
                    }
                }
                TypeDefinitionPosition::Interface(pos) => {
                    if type_directive_application.is_interface_object {
                        is_interface_object = true;
                        let interface_object_directive = federation_spec_definition
                            .interface_object_directive(&subgraph.schema)?;
                        let pos = ObjectTypeDefinitionPosition {
                            type_name: pos.type_name.clone(),
                        };
                        pos.pre_insert(&mut subgraph.schema)?;
                        pos.insert(
                            &mut subgraph.schema,
                            Node::new(ObjectType {
                                description: None,
                                name: pos.type_name.clone(),
                                implements_interfaces: Default::default(),
                                directives: DirectiveList(vec![Component::new(
                                    interface_object_directive,
                                )]),
                                fields: Default::default(),
                            }),
                        )?;
                    } else {
                        pos.pre_insert(&mut subgraph.schema)?;
                        pos.insert(
                            &mut subgraph.schema,
                            Node::new(InterfaceType {
                                description: None,
                                name: pos.type_name.clone(),
                                implements_interfaces: Default::default(),
                                directives: Default::default(),
                                fields: Default::default(),
                            }),
                        )?;
                    }
                }
                TypeDefinitionPosition::Union(pos) => {
                    pos.pre_insert(&mut subgraph.schema)?;
                    pos.insert(
                        &mut subgraph.schema,
                        Node::new(UnionType {
                            description: None,
                            name: pos.type_name.clone(),
                            directives: Default::default(),
                            members: Default::default(),
                        }),
                    )?;
                }
                TypeDefinitionPosition::Enum(pos) => {
                    pos.pre_insert(&mut subgraph.schema)?;
                    pos.insert(
                        &mut subgraph.schema,
                        Node::new(EnumType {
                            description: None,
                            name: pos.type_name.clone(),
                            directives: Default::default(),
                            values: Default::default(),
                        }),
                    )?;
                }
                TypeDefinitionPosition::InputObject(pos) => {
                    pos.pre_insert(&mut subgraph.schema)?;
                    pos.insert(
                        &mut subgraph.schema,
                        Node::new(InputObjectType {
                            description: None,
                            name: pos.type_name.clone(),
                            directives: Default::default(),
                            fields: Default::default(),
                        }),
                    )?;
                }
            };
            type_info.subgraph_info.insert(
                type_directive_application.graph.clone(),
                is_interface_object,
            );
        }

        if let Some(key) = &type_directive_application.key {
            let mut key_directive = Component::new(federation_spec_definition.key_directive(
                &subgraph.schema,
                key,
                type_directive_application.resolvable,
            )?);
            if type_directive_application.extension {
                key_directive.origin =
                    ComponentOrigin::Extension(ExtensionId::new(&key_directive.node))
            }
            let subgraph_type_definition_position = subgraph
                .schema
                .get_type(type_definition_position.type_name().clone())?;
            match &subgraph_type_definition_position {
                TypeDefinitionPosition::Scalar(_) => {
                    return Err(SingleFederationError::Internal {
                        message: "\"add_empty_type()\" shouldn't be called for scalars".to_owned(),
                    }
                    .into());
                }
                TypeDefinitionPosition::Object(pos) => {
                    pos.insert_directive(&mut subgraph.schema, key_directive)?;
                }
                TypeDefinitionPosition::Interface(pos) => {
                    pos.insert_directive(&mut subgraph.schema, key_directive)?;
                }
                TypeDefinitionPosition::Union(pos) => {
                    pos.insert_directive(&mut subgraph.schema, key_directive)?;
                }
                TypeDefinitionPosition::Enum(pos) => {
                    pos.insert_directive(&mut subgraph.schema, key_directive)?;
                }
                TypeDefinitionPosition::InputObject(pos) => {
                    pos.insert_directive(&mut subgraph.schema, key_directive)?;
                }
            };
        }
    }

    Ok(type_info)
}

fn extract_object_type_content(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
    join_spec_definition: &JoinSpecDefinition,
    info: &[TypeInfo],
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<(), FederationError> {
    let field_directive_definition =
        join_spec_definition.field_directive_definition(supergraph_schema)?;
    // join__implements was added in join 0.2, and this method does not run for join 0.1, so it
    // should be defined.
    let implements_directive_definition = join_spec_definition
        .implements_directive_definition(supergraph_schema)?
        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
            message: "@join__implements should exist for a fed2 supergraph".to_owned(),
        })?;

    for TypeInfo {
        name: type_name,
        subgraph_info,
    } in info.iter()
    {
        let pos = ObjectTypeDefinitionPosition {
            type_name: (*type_name).clone(),
        };
        let type_ = pos.get(supergraph_schema.schema())?;

        for directive in type_
            .directives
            .get_all(&implements_directive_definition.name)
        {
            let implements_directive_application =
                join_spec_definition.implements_directive_arguments(directive)?;
            if !subgraph_info.contains_key(&implements_directive_application.graph) {
                return Err(
                    SingleFederationError::InvalidFederationSupergraph {
                        message: format!(
                            "@join__implements cannot exist on \"{}\" for subgraph \"{}\" without type-level @join__type",
                            type_name,
                            implements_directive_application.graph,
                        ),
                    }.into()
                );
            }
            let subgraph = get_subgraph(
                subgraphs,
                graph_enum_value_name_to_subgraph_name,
                &implements_directive_application.graph,
            )?;
            pos.insert_implements_interface(
                &mut subgraph.schema,
                ComponentName::from(Name::new(implements_directive_application.interface)?),
            )?;
        }

        for graph_enum_value in subgraph_info.keys() {
            let subgraph = get_subgraph(
                subgraphs,
                graph_enum_value_name_to_subgraph_name,
                graph_enum_value,
            )?;
            let federation_spec_definition = federation_spec_definitions
                .get(graph_enum_value)
                .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                    message: "Subgraph unexpectedly does not use federation spec".to_owned(),
                })?;
            if let Some(cost_spec_definition) =
                federation_spec_definition.get_cost_spec_definition(&subgraph.schema)
            {
                cost_spec_definition.propagate_demand_control_directives_for_object(
                    &mut subgraph.schema,
                    type_,
                    &pos,
                    original_directive_names,
                )?;
            }
        }

        for (field_name, field) in type_.fields.iter() {
            let field_pos = pos.field(field_name.clone());
            let mut field_directive_applications = Vec::new();
            for directive in field.directives.get_all(&field_directive_definition.name) {
                field_directive_applications
                    .push(join_spec_definition.field_directive_arguments(directive)?);
            }
            if field_directive_applications.is_empty() {
                // In a fed2 subgraph, no @join__field means that the field is in all the subgraphs
                // in which the type is.
                let is_shareable = subgraph_info.len() > 1;
                for graph_enum_value in subgraph_info.keys() {
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(graph_enum_value)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;
                    let cost_spec_definition =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema);
                    add_subgraph_field(
                        field_pos.clone().into(),
                        field,
                        subgraph,
                        federation_spec_definition,
                        is_shareable,
                        None,
                        cost_spec_definition,
                        original_directive_names,
                    )?;
                }
            } else {
                let is_shareable = field_directive_applications
                    .iter()
                    .filter(|field_directive_application| {
                        !field_directive_application.external.unwrap_or(false)
                            && !field_directive_application.user_overridden.unwrap_or(false)
                    })
                    .count()
                    > 1;

                for field_directive_application in &field_directive_applications {
                    let Some(graph_enum_value) = &field_directive_application.graph else {
                        // We use a @join__field with no graph to indicates when a field in the
                        // supergraph does not come directly from any subgraph and there is thus
                        // nothing to do to "extract" it.
                        continue;
                    };
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(graph_enum_value)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;
                    let cost_spec_definition =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema);
                    if !subgraph_info.contains_key(graph_enum_value) {
                        return Err(
                            SingleFederationError::InvalidFederationSupergraph {
                                message: format!(
                                    "@join__field cannot exist on {}.{} for subgraph {} without type-level @join__type",
                                    type_name,
                                    field_name,
                                    graph_enum_value,
                                ),
                            }.into()
                        );
                    }
                    add_subgraph_field(
                        field_pos.clone().into(),
                        field,
                        subgraph,
                        federation_spec_definition,
                        is_shareable,
                        Some(field_directive_application),
                        cost_spec_definition,
                        original_directive_names,
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn extract_interface_type_content(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
    join_spec_definition: &JoinSpecDefinition,
    info: &[TypeInfo],
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<(), FederationError> {
    let field_directive_definition =
        join_spec_definition.field_directive_definition(supergraph_schema)?;
    // join_implements was added in join 0.2, and this method does not run for join 0.1, so it
    // should be defined.
    let implements_directive_definition = join_spec_definition
        .implements_directive_definition(supergraph_schema)?
        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
            message: "@join__implements should exist for a fed2 supergraph".to_owned(),
        })?;

    for TypeInfo {
        name: type_name,
        subgraph_info,
    } in info.iter()
    {
        let type_ = InterfaceTypeDefinitionPosition {
            type_name: (*type_name).clone(),
        }
        .get(supergraph_schema.schema())?;
        fn get_pos(
            subgraph: &FederationSubgraph,
            subgraph_info: &IndexMap<Name, bool>,
            graph_enum_value: &Name,
            type_name: NamedType,
        ) -> Result<ObjectOrInterfaceTypeDefinitionPosition, FederationError> {
            let is_interface_object = *subgraph_info.get(graph_enum_value).ok_or_else(|| {
                SingleFederationError::InvalidFederationSupergraph {
                    message: format!(
                        "@join__implements cannot exist on {} for subgraph {} without type-level @join__type",
                        type_name,
                        graph_enum_value,
                    ),
                }
            })?;
            Ok(match subgraph.schema.get_type(type_name.clone())? {
                TypeDefinitionPosition::Object(pos) => {
                    if !is_interface_object {
                        return Err(
                            SingleFederationError::Internal {
                                message: "\"extract_interface_type_content()\" encountered an unexpected interface object type in subgraph".to_owned(),
                            }.into()
                        );
                    }
                    pos.into()
                }
                TypeDefinitionPosition::Interface(pos) => {
                    if is_interface_object {
                        return Err(
                            SingleFederationError::Internal {
                                message: "\"extract_interface_type_content()\" encountered an interface type in subgraph that should have been an interface object".to_owned(),
                            }.into()
                        );
                    }
                    pos.into()
                }
                _ => {
                    return Err(
                        SingleFederationError::Internal {
                            message: "\"extract_interface_type_content()\" encountered non-object/interface type in subgraph".to_owned(),
                        }.into()
                    );
                }
            })
        }

        for directive in type_
            .directives
            .get_all(&implements_directive_definition.name)
        {
            let implements_directive_application =
                join_spec_definition.implements_directive_arguments(directive)?;
            let subgraph = get_subgraph(
                subgraphs,
                graph_enum_value_name_to_subgraph_name,
                &implements_directive_application.graph,
            )?;
            let pos = get_pos(
                subgraph,
                subgraph_info,
                &implements_directive_application.graph,
                type_name.clone(),
            )?;
            match pos {
                ObjectOrInterfaceTypeDefinitionPosition::Object(pos) => {
                    pos.insert_implements_interface(
                        &mut subgraph.schema,
                        ComponentName::from(Name::new(implements_directive_application.interface)?),
                    )?;
                }
                ObjectOrInterfaceTypeDefinitionPosition::Interface(pos) => {
                    pos.insert_implements_interface(
                        &mut subgraph.schema,
                        ComponentName::from(Name::new(implements_directive_application.interface)?),
                    )?;
                }
            }
        }

        for (field_name, field) in type_.fields.iter() {
            let mut field_directive_applications = Vec::new();
            for directive in field.directives.get_all(&field_directive_definition.name) {
                field_directive_applications
                    .push(join_spec_definition.field_directive_arguments(directive)?);
            }
            if field_directive_applications.is_empty() {
                // In a fed2 subgraph, no @join__field means that the field is in all the subgraphs
                // in which the type is.
                for graph_enum_value in subgraph_info.keys() {
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    let pos =
                        get_pos(subgraph, subgraph_info, graph_enum_value, type_name.clone())?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(graph_enum_value)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;
                    let cost_spec_definition =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema);
                    add_subgraph_field(
                        pos.field(field_name.clone()),
                        field,
                        subgraph,
                        federation_spec_definition,
                        false,
                        None,
                        cost_spec_definition,
                        original_directive_names,
                    )?;
                }
            } else {
                for field_directive_application in &field_directive_applications {
                    let Some(graph_enum_value) = &field_directive_application.graph else {
                        // We use a @join__field with no graph to indicates when a field in the
                        // supergraph does not come directly from any subgraph and there is thus
                        // nothing to do to "extract" it.
                        continue;
                    };
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    let pos =
                        get_pos(subgraph, subgraph_info, graph_enum_value, type_name.clone())?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(graph_enum_value)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;
                    let cost_spec_definition =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema);
                    if !subgraph_info.contains_key(graph_enum_value) {
                        return Err(
                            SingleFederationError::InvalidFederationSupergraph {
                                message: format!(
                                    "@join__field cannot exist on {}.{} for subgraph {} without type-level @join__type",
                                    type_name,
                                    field_name,
                                    graph_enum_value,
                                ),
                            }.into()
                        );
                    }
                    add_subgraph_field(
                        pos.field(field_name.clone()),
                        field,
                        subgraph,
                        federation_spec_definition,
                        false,
                        Some(field_directive_application),
                        cost_spec_definition,
                        original_directive_names,
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn extract_union_type_content(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    join_spec_definition: &JoinSpecDefinition,
    info: &[TypeInfo],
) -> Result<(), FederationError> {
    // This was added in join 0.3, so it can genuinely be None.
    let union_member_directive_definition =
        join_spec_definition.union_member_directive_definition(supergraph_schema)?;

    // Note that union members works a bit differently from fields or enum values, and this because
    // we cannot have directive applications on type members. So the `join_unionMember` directive
    // applications are on the type itself, and they mention the member that they target.
    for TypeInfo {
        name: type_name,
        subgraph_info,
    } in info.iter()
    {
        let pos = UnionTypeDefinitionPosition {
            type_name: (*type_name).clone(),
        };
        let type_ = pos.get(supergraph_schema.schema())?;

        let mut union_member_directive_applications = Vec::new();
        if let Some(union_member_directive_definition) = union_member_directive_definition {
            for directive in type_
                .directives
                .get_all(&union_member_directive_definition.name)
            {
                union_member_directive_applications
                    .push(join_spec_definition.union_member_directive_arguments(directive)?);
            }
        }
        if union_member_directive_applications.is_empty() {
            // No @join__unionMember; every member should be added to every subgraph having the
            // union (at least as long as the subgraph has the member itself).
            for graph_enum_value in subgraph_info.keys() {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    graph_enum_value,
                )?;
                // Note that object types in the supergraph are guaranteed to be object types in
                // subgraphs.
                let subgraph_members = type_
                    .members
                    .iter()
                    .filter(|member| {
                        subgraph
                            .schema
                            .schema()
                            .types
                            .contains_key((*member).deref())
                    })
                    .collect::<Vec<_>>();
                for member in subgraph_members {
                    pos.insert_member(&mut subgraph.schema, ComponentName::from(&member.name))?;
                }
            }
        } else {
            for union_member_directive_application in &union_member_directive_applications {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &union_member_directive_application.graph,
                )?;
                if !subgraph_info.contains_key(&union_member_directive_application.graph) {
                    return Err(
                        SingleFederationError::InvalidFederationSupergraph {
                            message: format!(
                                "@join__unionMember cannot exist on {} for subgraph {} without type-level @join__type",
                                type_name,
                                union_member_directive_application.graph,
                            ),
                        }.into()
                    );
                }
                // Note that object types in the supergraph are guaranteed to be object types in
                // subgraphs. We also know that the type must exist in this case (we don't generate
                // broken @join__unionMember).
                pos.insert_member(
                    &mut subgraph.schema,
                    ComponentName::from(Name::new(union_member_directive_application.member)?),
                )?;
            }
        }
    }

    Ok(())
}

fn extract_enum_type_content(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
    join_spec_definition: &JoinSpecDefinition,
    info: &[TypeInfo],
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<(), FederationError> {
    // This was added in join 0.3, so it can genuinely be None.
    let enum_value_directive_definition =
        join_spec_definition.enum_value_directive_definition(supergraph_schema)?;

    for TypeInfo {
        name: type_name,
        subgraph_info,
    } in info.iter()
    {
        let pos = EnumTypeDefinitionPosition {
            type_name: (*type_name).clone(),
        };
        let type_ = pos.get(supergraph_schema.schema())?;

        for graph_enum_value in subgraph_info.keys() {
            let subgraph = get_subgraph(
                subgraphs,
                graph_enum_value_name_to_subgraph_name,
                graph_enum_value,
            )?;
            let federation_spec_definition = federation_spec_definitions
                .get(graph_enum_value)
                .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                    message: "Subgraph unexpectedly does not use federation spec".to_owned(),
                })?;
            if let Some(cost_spec_definition) =
                federation_spec_definition.get_cost_spec_definition(&subgraph.schema)
            {
                cost_spec_definition.propagate_demand_control_directives_for_enum(
                    &mut subgraph.schema,
                    type_,
                    &pos,
                    original_directive_names,
                )?;
            }
        }

        for (value_name, value) in type_.values.iter() {
            let value_pos = pos.value(value_name.clone());
            let mut enum_value_directive_applications = Vec::new();
            if let Some(enum_value_directive_definition) = enum_value_directive_definition {
                for directive in value
                    .directives
                    .get_all(&enum_value_directive_definition.name)
                {
                    enum_value_directive_applications
                        .push(join_spec_definition.enum_value_directive_arguments(directive)?);
                }
            }
            if enum_value_directive_applications.is_empty() {
                for graph_enum_value in subgraph_info.keys() {
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    value_pos.insert(
                        &mut subgraph.schema,
                        Component::new(EnumValueDefinition {
                            description: None,
                            value: value_name.clone(),
                            directives: Default::default(),
                        }),
                    )?;
                }
            } else {
                for enum_value_directive_application in &enum_value_directive_applications {
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        &enum_value_directive_application.graph,
                    )?;
                    if !subgraph_info.contains_key(&enum_value_directive_application.graph) {
                        return Err(
                            SingleFederationError::InvalidFederationSupergraph {
                                message: format!(
                                    "@join__enumValue cannot exist on {}.{} for subgraph {} without type-level @join__type",
                                    type_name,
                                    value_name,
                                    enum_value_directive_application.graph,
                                ),
                            }.into()
                        );
                    }
                    value_pos.insert(
                        &mut subgraph.schema,
                        Component::new(EnumValueDefinition {
                            description: None,
                            value: value_name.clone(),
                            directives: Default::default(),
                        }),
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn extract_input_object_type_content(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    federation_spec_definitions: &IndexMap<Name, &'static FederationSpecDefinition>,
    join_spec_definition: &JoinSpecDefinition,
    info: &[TypeInfo],
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<(), FederationError> {
    let field_directive_definition =
        join_spec_definition.field_directive_definition(supergraph_schema)?;

    for TypeInfo {
        name: type_name,
        subgraph_info,
    } in info.iter()
    {
        let pos = InputObjectTypeDefinitionPosition {
            type_name: (*type_name).clone(),
        };
        let type_ = pos.get(supergraph_schema.schema())?;

        for (input_field_name, input_field) in type_.fields.iter() {
            let input_field_pos = pos.field(input_field_name.clone());
            let mut field_directive_applications = Vec::new();
            for directive in input_field
                .directives
                .get_all(&field_directive_definition.name)
            {
                field_directive_applications
                    .push(join_spec_definition.field_directive_arguments(directive)?);
            }
            if field_directive_applications.is_empty() {
                for graph_enum_value in subgraph_info.keys() {
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(graph_enum_value)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;
                    let cost_spec_definition =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema);
                    add_subgraph_input_field(
                        input_field_pos.clone(),
                        input_field,
                        subgraph,
                        None,
                        cost_spec_definition,
                        original_directive_names,
                    )?;
                }
            } else {
                for field_directive_application in &field_directive_applications {
                    let Some(graph_enum_value) = &field_directive_application.graph else {
                        // We use a @join__field with no graph to indicates when a field in the
                        // supergraph does not come directly from any subgraph and there is thus
                        // nothing to do to "extract" it.
                        continue;
                    };
                    let subgraph = get_subgraph(
                        subgraphs,
                        graph_enum_value_name_to_subgraph_name,
                        graph_enum_value,
                    )?;
                    let federation_spec_definition = federation_spec_definitions
                        .get(graph_enum_value)
                        .ok_or_else(|| SingleFederationError::InvalidFederationSupergraph {
                            message: "Subgraph unexpectedly does not use federation spec"
                                .to_owned(),
                        })?;
                    let cost_spec_definition =
                        federation_spec_definition.get_cost_spec_definition(&subgraph.schema);
                    if !subgraph_info.contains_key(graph_enum_value) {
                        return Err(
                            SingleFederationError::InvalidFederationSupergraph {
                                message: format!(
                                    "@join__field cannot exist on {}.{} for subgraph {} without type-level @join__type",
                                    type_name,
                                    input_field_name,
                                    graph_enum_value,
                                ),
                            }.into()
                        );
                    }
                    add_subgraph_input_field(
                        input_field_pos.clone(),
                        input_field,
                        subgraph,
                        Some(field_directive_application),
                        cost_spec_definition,
                        original_directive_names,
                    )?;
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_subgraph_field(
    object_or_interface_field_definition_position: ObjectOrInterfaceFieldDefinitionPosition,
    field: &FieldDefinition,
    subgraph: &mut FederationSubgraph,
    federation_spec_definition: &'static FederationSpecDefinition,
    is_shareable: bool,
    field_directive_application: Option<&FieldDirectiveArguments>,
    cost_spec_definition: Option<&'static CostSpecDefinition>,
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<(), FederationError> {
    let field_directive_application =
        field_directive_application.unwrap_or_else(|| &FieldDirectiveArguments {
            graph: None,
            requires: None,
            provides: None,
            type_: None,
            external: None,
            override_: None,
            override_label: None,
            user_overridden: None,
        });
    let subgraph_field_type = match &field_directive_application.type_ {
        Some(t) => decode_type(t)?,
        None => field.ty.clone(),
    };
    let mut subgraph_field = FieldDefinition {
        description: None,
        name: object_or_interface_field_definition_position
            .field_name()
            .clone(),
        arguments: vec![],
        ty: subgraph_field_type,
        directives: Default::default(),
    };

    for argument in &field.arguments {
        let mut destination_argument = InputValueDefinition {
            description: None,
            name: argument.name.clone(),
            ty: argument.ty.clone(),
            default_value: argument.default_value.clone(),
            directives: Default::default(),
        };
        if let Some(cost_spec_definition) = cost_spec_definition {
            cost_spec_definition.propagate_demand_control_directives(
                &subgraph.schema,
                &argument.directives,
                &mut destination_argument.directives,
                original_directive_names,
            )?;
        }

        subgraph_field
            .arguments
            .push(Node::new(destination_argument))
    }
    if let Some(requires) = &field_directive_application.requires {
        subgraph_field.directives.push(Node::new(
            federation_spec_definition
                .requires_directive(&subgraph.schema, requires.to_string())?,
        ));
    }
    if let Some(provides) = &field_directive_application.provides {
        subgraph_field.directives.push(Node::new(
            federation_spec_definition
                .provides_directive(&subgraph.schema, provides.to_string())?,
        ));
    }
    let external = field_directive_application.external.unwrap_or(false);
    if external {
        subgraph_field.directives.push(Node::new(
            federation_spec_definition.external_directive(&subgraph.schema, None)?,
        ));
    }
    let user_overridden = field_directive_application.user_overridden.unwrap_or(false);
    if user_overridden {
        subgraph_field.directives.push(Node::new(
            federation_spec_definition
                .external_directive(&subgraph.schema, Some("[overridden]".to_string()))?,
        ));
    }
    if let Some(override_) = &field_directive_application.override_ {
        subgraph_field
            .directives
            .push(Node::new(federation_spec_definition.override_directive(
                &subgraph.schema,
                override_.to_string(),
                &field_directive_application.override_label,
            )?));
    }
    if is_shareable && !external && !user_overridden {
        subgraph_field.directives.push(Node::new(
            federation_spec_definition.shareable_directive(&subgraph.schema)?,
        ));
    }

    if let Some(cost_spec_definition) = cost_spec_definition {
        cost_spec_definition.propagate_demand_control_directives(
            &subgraph.schema,
            &field.directives,
            &mut subgraph_field.directives,
            original_directive_names,
        )?;
    }

    match object_or_interface_field_definition_position {
        ObjectOrInterfaceFieldDefinitionPosition::Object(pos) => {
            pos.insert(&mut subgraph.schema, Component::from(subgraph_field))?;
        }
        ObjectOrInterfaceFieldDefinitionPosition::Interface(pos) => {
            pos.insert(&mut subgraph.schema, Component::from(subgraph_field))?;
        }
    };

    Ok(())
}

fn add_subgraph_input_field(
    input_object_field_definition_position: InputObjectFieldDefinitionPosition,
    input_field: &InputValueDefinition,
    subgraph: &mut FederationSubgraph,
    field_directive_application: Option<&FieldDirectiveArguments>,
    cost_spec_definition: Option<&'static CostSpecDefinition>,
    original_directive_names: &IndexMap<Name, Name>,
) -> Result<(), FederationError> {
    let field_directive_application =
        field_directive_application.unwrap_or_else(|| &FieldDirectiveArguments {
            graph: None,
            requires: None,
            provides: None,
            type_: None,
            external: None,
            override_: None,
            override_label: None,
            user_overridden: None,
        });
    let subgraph_input_field_type = match &field_directive_application.type_ {
        Some(t) => Node::new(decode_type(t)?),
        None => input_field.ty.clone(),
    };
    let mut subgraph_input_field = InputValueDefinition {
        description: None,
        name: input_object_field_definition_position.field_name.clone(),
        ty: subgraph_input_field_type,
        default_value: input_field.default_value.clone(),
        directives: Default::default(),
    };

    if let Some(cost_spec_definition) = cost_spec_definition {
        cost_spec_definition.propagate_demand_control_directives(
            &subgraph.schema,
            &input_field.directives,
            &mut subgraph_input_field.directives,
            original_directive_names,
        )?;
    }

    input_object_field_definition_position
        .insert(&mut subgraph.schema, Component::from(subgraph_input_field))?;

    Ok(())
}

/// Parse a string encoding a type reference.
fn decode_type(type_: &str) -> Result<Type, FederationError> {
    Ok(Type::parse(type_, "")?)
}

fn get_subgraph<'subgraph>(
    subgraphs: &'subgraph mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
    graph_enum_value: &Name,
) -> Result<&'subgraph mut FederationSubgraph, FederationError> {
    let subgraph_name = graph_enum_value_name_to_subgraph_name
        .get(graph_enum_value)
        .ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Invalid graph enum_value \"{}\": does not match an enum value defined in the @join__Graph enum",
                    graph_enum_value,
                ),
            }
        })?;
    subgraphs.get_mut(subgraph_name).ok_or_else(|| {
        SingleFederationError::Internal {
            message: "All subgraphs should have been created by \"collect_empty_subgraphs()\""
                .to_owned(),
        }
        .into()
    })
}

struct FederationSubgraph {
    name: String,
    url: String,
    schema: FederationSchema,
}

struct FederationSubgraphs {
    subgraphs: BTreeMap<String, FederationSubgraph>,
}

impl FederationSubgraphs {
    fn new() -> Self {
        FederationSubgraphs {
            subgraphs: BTreeMap::new(),
        }
    }

    fn add(&mut self, subgraph: FederationSubgraph) -> Result<(), FederationError> {
        if self.subgraphs.contains_key(&subgraph.name) {
            return Err(SingleFederationError::InvalidFederationSupergraph {
                message: format!("A subgraph named \"{}\" already exists", subgraph.name),
            }
            .into());
        }
        self.subgraphs.insert(subgraph.name.clone(), subgraph);
        Ok(())
    }

    fn get(&self, name: &str) -> Option<&FederationSubgraph> {
        self.subgraphs.get(name)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut FederationSubgraph> {
        self.subgraphs.get_mut(name)
    }
}

impl IntoIterator for FederationSubgraphs {
    type Item = <BTreeMap<String, FederationSubgraph> as IntoIterator>::Item;
    type IntoIter = <BTreeMap<String, FederationSubgraph> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.subgraphs.into_iter()
    }
}

// TODO(@goto-bus-stop): consider an appropriate name for this in the public API
// TODO(@goto-bus-stop): should this exist separately from the `crate::subgraph::Subgraph` type?
#[derive(Debug, Clone)]
pub struct ValidFederationSubgraph {
    pub name: String,
    pub url: String,
    pub schema: ValidFederationSchema,
}

pub struct ValidFederationSubgraphs {
    subgraphs: BTreeMap<Arc<str>, ValidFederationSubgraph>,
}

impl fmt::Debug for ValidFederationSubgraphs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ValidFederationSubgraphs ")?;
        f.debug_map().entries(self.subgraphs.iter()).finish()
    }
}

impl ValidFederationSubgraphs {
    pub(crate) fn new() -> Self {
        ValidFederationSubgraphs {
            subgraphs: BTreeMap::new(),
        }
    }

    pub(crate) fn add(&mut self, subgraph: ValidFederationSubgraph) -> Result<(), FederationError> {
        if self.subgraphs.contains_key(subgraph.name.as_str()) {
            return Err(SingleFederationError::InvalidFederationSupergraph {
                message: format!("A subgraph named \"{}\" already exists", subgraph.name),
            }
            .into());
        }
        self.subgraphs
            .insert(subgraph.name.as_str().into(), subgraph);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ValidFederationSubgraph> {
        self.subgraphs.get(name)
    }
}

impl IntoIterator for ValidFederationSubgraphs {
    type Item = <BTreeMap<Arc<str>, ValidFederationSubgraph> as IntoIterator>::Item;
    type IntoIter = <BTreeMap<Arc<str>, ValidFederationSubgraph> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.subgraphs.into_iter()
    }
}

lazy_static! {
    static ref EXECUTABLE_DIRECTIVE_LOCATIONS: IndexSet<DirectiveLocation> = {
        [
            DirectiveLocation::Query,
            DirectiveLocation::Mutation,
            DirectiveLocation::Subscription,
            DirectiveLocation::Field,
            DirectiveLocation::FragmentDefinition,
            DirectiveLocation::FragmentSpread,
            DirectiveLocation::InlineFragment,
            DirectiveLocation::VariableDefinition,
        ]
        .into_iter()
        .collect()
    };
}

fn remove_unused_types_from_subgraph(schema: &mut FederationSchema) -> Result<(), FederationError> {
    // We now do an additional path on all types because we sometimes added types to subgraphs
    // without being sure that the subgraph had the type in the first place (especially with the
    // join 0.1 spec), and because we later might not have added any fields/members to said type,
    // they may be empty (indicating they clearly didn't belong to the subgraph in the first) and we
    // need to remove them. Note that need to do this _after_ the `add_external_fields()` call above
    // since it may have added (external) fields to some of the types.
    let mut type_definition_positions: Vec<TypeDefinitionPosition> = Vec::new();
    for (type_name, type_) in schema.schema().types.iter() {
        match type_ {
            ExtendedType::Object(type_) => {
                if type_.fields.is_empty() {
                    type_definition_positions.push(
                        ObjectTypeDefinitionPosition {
                            type_name: type_name.clone(),
                        }
                        .into(),
                    );
                }
            }
            ExtendedType::Interface(type_) => {
                if type_.fields.is_empty() {
                    type_definition_positions.push(
                        InterfaceTypeDefinitionPosition {
                            type_name: type_name.clone(),
                        }
                        .into(),
                    );
                }
            }
            ExtendedType::Union(type_) => {
                if type_.members.is_empty() {
                    type_definition_positions.push(
                        UnionTypeDefinitionPosition {
                            type_name: type_name.clone(),
                        }
                        .into(),
                    );
                }
            }
            ExtendedType::InputObject(type_) => {
                if type_.fields.is_empty() {
                    type_definition_positions.push(
                        InputObjectTypeDefinitionPosition {
                            type_name: type_name.clone(),
                        }
                        .into(),
                    );
                }
            }
            _ => {}
        }
    }

    // Note that we have to use remove_recursive() or this could leave the subgraph invalid. But if
    // the type was not in this subgraph, nothing that depends on it should be either.
    for position in type_definition_positions {
        match position {
            TypeDefinitionPosition::Object(position) => {
                position.remove_recursive(schema)?;
            }
            TypeDefinitionPosition::Interface(position) => {
                position.remove_recursive(schema)?;
            }
            TypeDefinitionPosition::Union(position) => {
                position.remove_recursive(schema)?;
            }
            TypeDefinitionPosition::InputObject(position) => {
                position.remove_recursive(schema)?;
            }
            _ => {
                return Err(SingleFederationError::Internal {
                    message: "Encountered type kind that shouldn't have been removed".to_owned(),
                }
                .into());
            }
        }
    }

    Ok(())
}

const FEDERATION_ANY_TYPE_NAME: Name = name!("_Any");
const FEDERATION_SERVICE_TYPE_NAME: Name = name!("_Service");
const FEDERATION_SDL_FIELD_NAME: Name = name!("sdl");
const FEDERATION_ENTITY_TYPE_NAME: Name = name!("_Entity");
const FEDERATION_SERVICE_FIELD_NAME: Name = name!("_service");
const FEDERATION_ENTITIES_FIELD_NAME: Name = name!("_entities");
pub(crate) const FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME: Name = name!("representations");
pub(crate) const FEDERATION_REPRESENTATIONS_VAR_NAME: Name = name!("representations");

const GRAPHQL_STRING_TYPE_NAME: Name = name!("String");
const GRAPHQL_QUERY_TYPE_NAME: Name = name!("Query");

const ANY_TYPE_SPEC: ScalarTypeSpecification = ScalarTypeSpecification {
    name: FEDERATION_ANY_TYPE_NAME,
};

const SERVICE_TYPE_SPEC: ObjectTypeSpecification = ObjectTypeSpecification {
    name: FEDERATION_SERVICE_TYPE_NAME,
    fields: |_schema| {
        [FieldSpecification {
            name: FEDERATION_SDL_FIELD_NAME,
            ty: Type::Named(GRAPHQL_STRING_TYPE_NAME),
            arguments: Default::default(),
        }]
        .into()
    },
};

const QUERY_TYPE_SPEC: ObjectTypeSpecification = ObjectTypeSpecification {
    name: GRAPHQL_QUERY_TYPE_NAME,
    fields: |_schema| Default::default(), // empty Query (fields should be added later)
};

// PORT_NOTE: The JS implementation gets the key directive definition from the schema,
// but we have it as a parameter.
fn collect_entity_members(
    schema: &FederationSchema,
    key_directive_definition: &Node<DirectiveDefinition>,
) -> IndexSet<ComponentName> {
    schema
        .schema()
        .types
        .iter()
        .filter_map(|(type_name, type_)| {
            let ExtendedType::Object(type_) = type_ else {
                return None;
            };
            if !type_.directives.has(&key_directive_definition.name) {
                return None;
            }
            Some(ComponentName::from(type_name))
        })
        .collect::<IndexSet<_>>()
}

fn add_federation_operations(
    subgraph: &mut FederationSubgraph,
    federation_spec_definition: &'static FederationSpecDefinition,
) -> Result<(), FederationError> {
    // the `_Any` and `_Service` Type
    ANY_TYPE_SPEC.check_or_add(&mut subgraph.schema)?;
    SERVICE_TYPE_SPEC.check_or_add(&mut subgraph.schema)?;

    // the `_Entity` Type
    let key_directive_definition =
        federation_spec_definition.key_directive_definition(&subgraph.schema)?;
    let entity_members = collect_entity_members(&subgraph.schema, key_directive_definition);
    let has_entity_type = !entity_members.is_empty();
    if has_entity_type {
        UnionTypeSpecification {
            name: FEDERATION_ENTITY_TYPE_NAME,
            members: |_| entity_members.clone(),
        }
        .check_or_add(&mut subgraph.schema)?;
    }

    // the `Query` Type
    let query_root_pos = SchemaRootDefinitionPosition {
        root_kind: SchemaRootDefinitionKind::Query,
    };
    if query_root_pos.try_get(subgraph.schema.schema()).is_none() {
        QUERY_TYPE_SPEC.check_or_add(&mut subgraph.schema)?;
        query_root_pos.insert(
            &mut subgraph.schema,
            ComponentName::from(QUERY_TYPE_SPEC.name),
        )?;
    }

    // `Query._entities` (optional)
    let query_root_type_name = query_root_pos.get(subgraph.schema.schema())?.name.clone();
    let entity_field_pos = ObjectFieldDefinitionPosition {
        type_name: query_root_type_name.clone(),
        field_name: FEDERATION_ENTITIES_FIELD_NAME,
    };
    if has_entity_type {
        entity_field_pos.insert(
            &mut subgraph.schema,
            Component::new(FieldDefinition {
                description: None,
                name: FEDERATION_ENTITIES_FIELD_NAME,
                arguments: vec![Node::new(InputValueDefinition {
                    description: None,
                    name: FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME,
                    ty: Node::new(Type::NonNullList(Box::new(Type::NonNullNamed(
                        FEDERATION_ANY_TYPE_NAME,
                    )))),
                    default_value: None,
                    directives: Default::default(),
                })],
                ty: Type::NonNullList(Box::new(Type::Named(FEDERATION_ENTITY_TYPE_NAME))),
                directives: Default::default(),
            }),
        )?;
    } else {
        entity_field_pos.remove(&mut subgraph.schema)?;
    }

    // `Query._service`
    ObjectFieldDefinitionPosition {
        type_name: query_root_type_name.clone(),
        field_name: FEDERATION_SERVICE_FIELD_NAME,
    }
    .insert(
        &mut subgraph.schema,
        Component::new(FieldDefinition {
            description: None,
            name: FEDERATION_SERVICE_FIELD_NAME,
            arguments: Vec::new(),
            ty: Type::NonNullNamed(FEDERATION_SERVICE_TYPE_NAME),
            directives: Default::default(),
        }),
    )?;

    Ok(())
}

/// It makes no sense to have a @requires/@provides on a non-external leaf field, and we usually
/// reject it during schema validation. But this function remove such fields for when:
///  1. We extract subgraphs from a Fed 1 supergraph, where such validations haven't been run.
///  2. Fed 1 subgraphs are upgraded to Fed 2 subgraphs.
///
/// The reason we do this (and generally reject it) is that such @requires/@provides have a negative
/// impact on later query planning, because it sometimes make us try type-exploding some interfaces
/// unnecessarily. Besides, if a usage adds something useless, there is a chance it hasn't fully
/// understood something, and warning about that fact through an error is more helpful.
fn remove_inactive_requires_and_provides_from_subgraph(
    supergraph_schema: &FederationSchema,
    schema: &mut FederationSchema,
) -> Result<(), FederationError> {
    let federation_spec_definition = get_federation_spec_definition_from_subgraph(schema)?;
    let requires_directive_definition_name = federation_spec_definition
        .requires_directive_definition(schema)?
        .name
        .clone();
    let provides_directive_definition_name = federation_spec_definition
        .provides_directive_definition(schema)?
        .name
        .clone();

    let mut object_or_interface_field_definition_positions: Vec<
        ObjectOrInterfaceFieldDefinitionPosition,
    > = vec![];
    for type_pos in schema.get_types() {
        // Ignore introspection types.
        if is_graphql_reserved_name(type_pos.type_name()) {
            continue;
        }

        // Ignore non-object/interface types.
        let Ok(type_pos): Result<ObjectOrInterfaceTypeDefinitionPosition, _> = type_pos.try_into()
        else {
            continue;
        };

        match type_pos {
            ObjectOrInterfaceTypeDefinitionPosition::Object(type_pos) => {
                object_or_interface_field_definition_positions.extend(
                    type_pos
                        .get(schema.schema())?
                        .fields
                        .keys()
                        .map(|field_name| type_pos.field(field_name.clone()).into()),
                )
            }
            ObjectOrInterfaceTypeDefinitionPosition::Interface(type_pos) => {
                object_or_interface_field_definition_positions.extend(
                    type_pos
                        .get(schema.schema())?
                        .fields
                        .keys()
                        .map(|field_name| type_pos.field(field_name.clone()).into()),
                )
            }
        };
    }

    for pos in object_or_interface_field_definition_positions {
        remove_inactive_applications(
            supergraph_schema,
            schema,
            federation_spec_definition,
            FieldSetDirectiveKind::Requires,
            &requires_directive_definition_name,
            pos.clone(),
        )?;
        remove_inactive_applications(
            supergraph_schema,
            schema,
            federation_spec_definition,
            FieldSetDirectiveKind::Provides,
            &provides_directive_definition_name,
            pos,
        )?;
    }

    Ok(())
}

enum FieldSetDirectiveKind {
    Provides,
    Requires,
}

fn remove_inactive_applications(
    supergraph_schema: &FederationSchema,
    schema: &mut FederationSchema,
    federation_spec_definition: &'static FederationSpecDefinition,
    directive_kind: FieldSetDirectiveKind,
    name_in_schema: &Name,
    object_or_interface_field_definition_position: ObjectOrInterfaceFieldDefinitionPosition,
) -> Result<(), FederationError> {
    let mut replacement_directives = Vec::new();
    let field = object_or_interface_field_definition_position.get(schema.schema())?;
    for directive in field.directives.get_all(name_in_schema) {
        let (fields, parent_type_pos, is_requires_field_set) = match directive_kind {
            FieldSetDirectiveKind::Provides => {
                let fields = federation_spec_definition
                    .provides_directive_arguments(directive)?
                    .fields;
                let parent_type_pos: CompositeTypeDefinitionPosition = schema
                    .get_type(field.ty.inner_named_type().clone())?
                    .try_into()?;
                (fields, parent_type_pos, false)
            }
            FieldSetDirectiveKind::Requires => {
                let fields = federation_spec_definition
                    .requires_directive_arguments(directive)?
                    .fields;
                let parent_type_pos: CompositeTypeDefinitionPosition =
                    object_or_interface_field_definition_position
                        .parent()
                        .clone()
                        .into();
                (fields, parent_type_pos, true)
            }
        };
        // TODO: The assume_valid_ref() here is non-ideal, in the sense that the error messages we
        // get back during field set parsing may not be user-friendly. We can't really validate the
        // schema here since the schema may not be fully valid when this function is called within
        // extract_subgraphs_from_supergraph() (it would also incur significant performance loss).
        // At best, we could try to shift this computation to after the subgraph schema validation
        // step, but its unclear at this time whether performing this shift affects correctness (and
        // it takes time to determine that). So for now, we keep this here.
        // TODO: In the JS codebase, this function ends up getting additionally used in the schema
        // upgrader, where parsing the field set may error. In such cases, we end up skipping those
        // directives instead of returning error here, as it pollutes the list of error messages
        // during composition (another site in composition will properly check for field set
        // validity and give better error messaging).
        let mut fields = if is_requires_field_set {
            // @requires needs to be validated against the supergraph schema
            let valid_supergraph_schema = Valid::assume_valid_ref(supergraph_schema.schema());
            parse_field_set_without_normalization(
                valid_supergraph_schema,
                parent_type_pos.type_name().clone(),
                fields,
            )
        } else {
            let valid_schema = Valid::assume_valid_ref(schema.schema());
            parse_field_set_without_normalization(
                valid_schema,
                parent_type_pos.type_name().clone(),
                fields,
            )
        }?;
        let is_modified = remove_non_external_leaf_fields(schema, &mut fields)?;
        if is_modified {
            let replacement_directive = if fields.selections.is_empty() {
                None
            } else {
                let fields = fields.serialize().no_indent().to_string();
                Some(Node::new(match directive_kind {
                    FieldSetDirectiveKind::Provides => {
                        federation_spec_definition.provides_directive(schema, fields)?
                    }
                    FieldSetDirectiveKind::Requires => {
                        federation_spec_definition.requires_directive(schema, fields)?
                    }
                }))
            };
            replacement_directives.push((directive.clone(), replacement_directive))
        }
    }

    for (old_directive, new_directive) in replacement_directives {
        object_or_interface_field_definition_position.remove_directive(schema, &old_directive);
        if let Some(new_directive) = new_directive {
            object_or_interface_field_definition_position
                .insert_directive(schema, new_directive)?;
        }
    }
    Ok(())
}

/// Removes any non-external leaf fields from the selection set, returning true if the selection
/// set was modified.
fn remove_non_external_leaf_fields(
    schema: &FederationSchema,
    selection_set: &mut executable::SelectionSet,
) -> Result<bool, FederationError> {
    let federation_spec_definition = get_federation_spec_definition_from_subgraph(schema)?;
    let external_directive_definition_name = federation_spec_definition
        .external_directive_definition(schema)?
        .name
        .clone();
    remove_non_external_leaf_fields_internal(
        schema,
        &external_directive_definition_name,
        selection_set,
    )
}

fn remove_non_external_leaf_fields_internal(
    schema: &FederationSchema,
    external_directive_definition_name: &Name,
    selection_set: &mut executable::SelectionSet,
) -> Result<bool, FederationError> {
    let mut is_modified = false;
    let mut errors = MultipleFederationErrors { errors: Vec::new() };
    selection_set.selections.retain_mut(|selection| {
        let child_selection_set = match selection {
            executable::Selection::Field(field) => {
                match is_external_or_has_external_implementations(
                    schema,
                    external_directive_definition_name,
                    &selection_set.ty,
                    field,
                ) {
                    Ok(is_external) => {
                        if is_external {
                            // Either the field or one of its implementors is external, so we keep
                            // the entire selection in that case.
                            return true;
                        }
                    }
                    Err(error) => {
                        errors.push(error);
                        return false;
                    }
                };
                if field.selection_set.selections.is_empty() {
                    // An empty selection set means this is a leaf field. We would have returned
                    // earlier if this were external, so this is a non-external leaf field.
                    is_modified = true;
                    return false;
                }
                &mut field.make_mut().selection_set
            }
            executable::Selection::InlineFragment(inline_fragment) => {
                &mut inline_fragment.make_mut().selection_set
            }
            executable::Selection::FragmentSpread(_) => {
                errors.push(
                    SingleFederationError::Internal {
                        message: "Unexpectedly found named fragment in FieldSet scalar".to_owned(),
                    }
                    .into(),
                );
                return false;
            }
        };
        // At this point, we either have a non-leaf non-external field, or an inline fragment. In
        // either case, we recurse into its selection set.
        match remove_non_external_leaf_fields_internal(
            schema,
            external_directive_definition_name,
            child_selection_set,
        ) {
            Ok(is_child_modified) => {
                if is_child_modified {
                    is_modified = true;
                }
            }
            Err(error) => {
                errors.push(error);
                return false;
            }
        }
        // If the recursion resulted in the selection set becoming empty, we remove this selection.
        // Note that it shouldn't have started out empty, so if it became empty, is_child_modified
        // would have been true, which means is_modified has already been set appropriately.
        !child_selection_set.selections.is_empty()
    });
    if errors.errors.is_empty() {
        Ok(is_modified)
    } else {
        Err(errors.into())
    }
}

fn is_external_or_has_external_implementations(
    schema: &FederationSchema,
    external_directive_definition_name: &Name,
    parent_type_name: &NamedType,
    selection: &Node<executable::Field>,
) -> Result<bool, FederationError> {
    let type_pos: CompositeTypeDefinitionPosition =
        schema.get_type(parent_type_name.clone())?.try_into()?;
    let field_pos = type_pos.field(selection.name.clone())?;
    let field = field_pos.get(schema.schema())?;
    if field.directives.has(external_directive_definition_name) {
        return Ok(true);
    }
    if let FieldDefinitionPosition::Interface(field_pos) = field_pos {
        for runtime_object_pos in schema.possible_runtime_types(field_pos.parent().into())? {
            let runtime_field_pos = runtime_object_pos.field(field_pos.field_name.clone());
            let runtime_field = runtime_field_pos.get(schema.schema())?;
            if runtime_field
                .directives
                .has(external_directive_definition_name)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

static DEBUG_SUBGRAPHS_ENV_VARIABLE_NAME: &str = "APOLLO_FEDERATION_DEBUG_SUBGRAPHS";

fn maybe_dump_subgraph_schema(subgraph: FederationSubgraph, message: &mut String) {
    // NOTE: The std::fmt::write returns an error, but writing to a string will never return an
    // error, so the result is dropped.
    _ = match std::env::var(DEBUG_SUBGRAPHS_ENV_VARIABLE_NAME).map(|v| v.parse::<bool>()) {
        Ok(Ok(true)) => {
            let time = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
            let filename = format!("extracted-subgraph-{}-{time}.graphql", subgraph.name,);
            let contents = subgraph.schema.schema().to_string();
            match std::fs::write(&filename, contents) {
                Ok(_) => write!(
                    message,
                    "The (invalid) extracted subgraph has been written in: {filename}."
                ),
                Err(e) => write!(
                    message,
                    r#"Was not able to print generated subgraph for "{}" because: {e}"#,
                    subgraph.name
                ),
            }
        }
        _ => write!(
            message,
            "Re-run with environment variable '{}' set to 'true' to extract the invalid subgraph",
            DEBUG_SUBGRAPHS_ENV_VARIABLE_NAME
        ),
    };
}

////////////////////////////////////////////////////////////////////////////////
/// @join__directive extraction

static JOIN_DIRECTIVE: &str = "join__directive";

/// Converts `@join__directive(graphs: [A], name: "foo")` to `@foo` in the A subgraph.
/// If the directive is a link directive on the schema definition, we also need
/// to update the metadata and add the imported definitions.
fn extract_join_directives(
    supergraph_schema: &FederationSchema,
    subgraphs: &mut FederationSubgraphs,
    graph_enum_value_name_to_subgraph_name: &IndexMap<Name, Arc<str>>,
) -> Result<(), FederationError> {
    let join_directives = match supergraph_schema
        .referencers()
        .get_directive(JOIN_DIRECTIVE)
    {
        Ok(directives) => directives,
        Err(_) => {
            // No join directives found, nothing to do.
            return Ok(());
        }
    };

    if let Some(schema_def_pos) = &join_directives.schema {
        let schema_def = schema_def_pos.get(supergraph_schema.schema());
        let directives = schema_def
            .directives
            .iter()
            .filter_map(|d| {
                if d.name == JOIN_DIRECTIVE {
                    Some(join_directive_to_real_directive(d))
                } else {
                    None
                }
            })
            .collect_vec();

        // TODO: Do we need to handle the link directive being renamed?
        let (links, others) = directives
            .into_iter()
            .partition::<Vec<_>, _>(|(d, _)| d.name == DEFAULT_LINK_NAME);

        // After adding links, we'll check the link against a safelist of
        // specs and check_or_add the spec definitions if necessary.
        for (link_directive, subgraph_enum_values) in links {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                schema_def_pos.insert_directive(
                    &mut subgraph.schema,
                    Component::new(link_directive.clone()),
                )?;

                // TODO: add imported definitions from relevant specs
            }
        }

        // Other directives are added normally.
        for (directive, subgraph_enum_values) in others {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                schema_def_pos
                    .insert_directive(&mut subgraph.schema, Component::new(directive.clone()))?;
            }
        }
    }

    for object_field_pos in &join_directives.object_fields {
        let object_field = object_field_pos.get(supergraph_schema.schema())?;
        let directives = object_field
            .directives
            .iter()
            .filter_map(|d| {
                if d.name == JOIN_DIRECTIVE {
                    Some(join_directive_to_real_directive(d))
                } else {
                    None
                }
            })
            .collect_vec();

        for (directive, subgraph_enum_values) in directives {
            for subgraph_enum_value in subgraph_enum_values {
                let subgraph = get_subgraph(
                    subgraphs,
                    graph_enum_value_name_to_subgraph_name,
                    &subgraph_enum_value,
                )?;

                object_field_pos
                    .insert_directive(&mut subgraph.schema, Node::new(directive.clone()))?;
            }
        }
    }

    // TODO
    // - join_directives.directive_arguments
    // - join_directives.enum_types
    // - join_directives.enum_values
    // - join_directives.input_object_fields
    // - join_directives.input_object_types
    // - join_directives.interface_field_arguments
    // - join_directives.interface_fields
    // - join_directives.interface_types
    // - join_directives.object_field_arguments
    // - join_directives.object_types
    // - join_directives.scalar_types
    // - join_directives.union_types

    Ok(())
}

fn join_directive_to_real_directive(directive: &Node<Directive>) -> (Directive, Vec<Name>) {
    let subgraph_enum_values = directive
        .argument_by_name("graphs")
        .and_then(|arg| arg.as_list())
        .map(|list| {
            list.iter()
                .map(|node| {
                    Name::new(
                        node.as_enum()
                            .expect("join__directive(graphs:) value is an enum")
                            .as_str(),
                    )
                    .expect("join__directive(graphs:) value is a valid name")
                })
                .collect()
        })
        .expect("join__directive(graphs:) missing");

    let name = directive
        .argument_by_name("name")
        .expect("join__directive(name:) is present")
        .as_str()
        .expect("join__directive(name:) is a string");

    let arguments = directive
        .argument_by_name("args")
        .and_then(|a| a.as_object())
        .map(|args| {
            args.iter()
                .map(|(k, v)| {
                    Argument {
                        name: k.clone(),
                        value: v.clone(),
                    }
                    .into()
                })
                .collect()
        })
        .unwrap_or_default();

    let directive = Directive {
        name: Name::new(name).expect("join__directive(name:) is a valid name"),
        arguments,
    };

    (directive, subgraph_enum_values)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use crate::schema::FederationSchema;
    use crate::ValidFederationSubgraphs;

    // JS PORT NOTE: these tests were ported from
    // https://github.com/apollographql/federation/blob/3e2c845c74407a136b9e0066e44c1ad1467d3013/internals-js/src/__tests__/extractSubgraphsFromSupergraph.test.ts

    #[test]
    fn handles_types_having_no_fields_referenced_by_other_interfaces_in_a_subgraph_correctly() {
        /*
         * JS PORT NOTE: the original test used a Federation 1 supergraph.
         * The following supergraph has been generated from:

        federation_version: =2.6.0
        subgraphs:
            a:
                routing_url: http://a
                schema:
                    sdl: |
                        type Query {
                            q: A
                        }

                        interface A {
                            a: B
                        }

                        type B {
                            b: C @provides(fields: "c")
                        }

                        type C {
                            c: String
                        }
            b:
                routing_url: http://b
                schema:
                    sdl: |
                        type C {
                            c: String
                        }
            c:
                routing_url: http://c
                schema:
                    sdl: |
                        type D {
                            d: String
                        }

         * This tests is almost identical to the 'handles types having no fields referenced by other objects in a subgraph correctly'
         * one, except that the reference to the type being removed is in an interface, to make double-sure this case is
         * handled as well.
         */

        let supergraph = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
              query: Query
            }

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            interface A
              @join__type(graph: A)
            {
              a: B
            }

            type B
              @join__type(graph: A)
            {
              b: C
            }

            type C
              @join__type(graph: A)
              @join__type(graph: B)
            {
              c: String
            }

            type D
              @join__type(graph: C)
            {
              d: String
            }

            scalar join__FieldSet

            enum join__Graph {
              A @join__graph(name: "a", url: "http://a")
              B @join__graph(name: "b", url: "http://b")
              C @join__graph(name: "c", url: "http://c")
            }

            scalar link__Import

            enum link__Purpose {
              """
              `SECURITY` features provide metadata necessary to securely resolve fields.
              """
              SECURITY

              """
              `EXECUTION` features provide metadata necessary for operation execution.
              """
              EXECUTION
            }

            type Query
              @join__type(graph: A)
              @join__type(graph: B)
              @join__type(graph: C)
            {
              q: A @join__field(graph: A)
            }
        "#;

        let schema = Schema::parse(supergraph, "supergraph.graphql").unwrap();
        let ValidFederationSubgraphs { subgraphs } = super::extract_subgraphs_from_supergraph(
            &FederationSchema::new(schema).unwrap(),
            Some(true),
        )
        .unwrap();

        assert_eq!(subgraphs.len(), 3);

        let a = subgraphs.get("a").unwrap();
        // JS PORT NOTE: the original tests used the equivalent of `get_type`,
        // so we have to be careful about using `get_interface` here.
        assert!(a.schema.schema().get_interface("A").is_some());
        assert!(a.schema.schema().get_object("B").is_some());

        let b = subgraphs.get("b").unwrap();
        assert!(b.schema.schema().get_interface("A").is_none());
        assert!(b.schema.schema().get_object("B").is_none());

        let c = subgraphs.get("c").unwrap();
        assert!(c.schema.schema().get_interface("A").is_none());
        assert!(c.schema.schema().get_object("B").is_none());
    }

    #[test]
    fn handles_types_having_no_fields_referenced_by_other_unions_in_a_subgraph_correctly() {
        /*
         * JS PORT NOTE: the original test used a Federation 1 supergraph.
         * The following supergraph has been generated from:

        federation_version: =2.6.0
        subgraphs:
            a:
                routing_url: http://a
                schema:
                    sdl: |
                        type Query {
                            q: A
                        }

                        union A = B | C

                        type B {
                            b: D @provides(fields: "d")
                        }

                        type C {
                            c: D @provides(fields: "d")
                        }

                        type D {
                            d: String
                        }
            b:
                routing_url: http://b
                schema:
                    sdl: |
                        type D {
                            d: String
                        }

         * This tests is similar identical to 'handles types having no fields referenced by other objects in a subgraph correctly'
         * but the reference to the type being removed is a union, one that should be fully removed.
         */

        let supergraph = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
              query: Query
            }

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            union A
              @join__type(graph: A)
              @join__unionMember(graph: A, member: "B")
              @join__unionMember(graph: A, member: "C")
             = B | C

            type B
              @join__type(graph: A)
            {
              b: D
            }

            type C
              @join__type(graph: A)
            {
              c: D
            }

            type D
              @join__type(graph: A)
              @join__type(graph: B)
            {
              d: String
            }

            scalar join__FieldSet

            enum join__Graph {
              A @join__graph(name: "a", url: "http://a")
              B @join__graph(name: "b", url: "http://b")
            }

            scalar link__Import

            enum link__Purpose {
              """
              `SECURITY` features provide metadata necessary to securely resolve fields.
              """
              SECURITY

              """
              `EXECUTION` features provide metadata necessary for operation execution.
              """
              EXECUTION
            }

            type Query
              @join__type(graph: A)
              @join__type(graph: B)
            {
              q: A @join__field(graph: A)
            }
        "#;

        let schema = Schema::parse(supergraph, "supergraph.graphql").unwrap();
        let ValidFederationSubgraphs { subgraphs } = super::extract_subgraphs_from_supergraph(
            &FederationSchema::new(schema).unwrap(),
            Some(true),
        )
        .unwrap();

        assert_eq!(subgraphs.len(), 2);

        let a = subgraphs.get("a").unwrap();
        // JS PORT NOTE: the original tests used the equivalent of `get_type`,
        // so we have to be careful about using `get_union` here.
        assert!(a.schema.schema().get_union("A").is_some());
        assert!(a.schema.schema().get_object("B").is_some());
        assert!(a.schema.schema().get_object("C").is_some());
        assert!(a.schema.schema().get_object("D").is_some());

        let b = subgraphs.get("b").unwrap();
        assert!(b.schema.schema().get_union("A").is_none());
        assert!(b.schema.schema().get_object("B").is_none());
        assert!(b.schema.schema().get_object("C").is_none());
        assert!(b.schema.schema().get_object("D").is_some());
    }

    // JS PORT NOTE: the "handles types having only some of their fields removed in a subgraph correctly"
    // test isn't relevant to Federation 2 supergraphs. Fed 1 supergraphs don't annotate all types with
    // the associated subgraphs, so extraction sometimes required guessing about which types to bring
    // into each subgraph.

    #[test]
    fn handles_unions_types_having_no_members_in_a_subgraph_correctly() {
        /*
         * JS PORT NOTE: the original test used a Federation 1 supergraph.
         * The following supergraph has been generated from:

        federation_version: =2.6.0
        subgraphs:
            a:
                routing_url: http://a
                schema:
                    sdl: |
                        type Query {
                            q: A
                        }

                        union A = B | C

                        type B @key(fields: "b { d }") {
                            b: D
                        }

                        type C @key(fields: "c { d }") {
                            c: D
                        }

                        type D {
                            d: String
                        }
            b:
                routing_url: http://b
                schema:
                    sdl: |
                        type D {
                            d: String
                        }

         * This tests is similar to the other test with unions, but because its members are enties, the
         * members themself with have a join__owner, and that means the removal will hit a different
         * code path (technically, the union A will be "removed" directly by `extractSubgraphsFromSupergraph`
         * instead of being removed indirectly through the removal of its members).
         */

        let supergraph = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
              query: Query
            }

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            union A
              @join__type(graph: A)
              @join__unionMember(graph: A, member: "B")
              @join__unionMember(graph: A, member: "C")
             = B | C

            type B
              @join__type(graph: A, key: "b { d }")
            {
              b: D
            }

            type C
              @join__type(graph: A, key: "c { d }")
            {
              c: D
            }

            type D
              @join__type(graph: A)
              @join__type(graph: B)
            {
              d: String
            }

            scalar join__FieldSet

            enum join__Graph {
              A @join__graph(name: "a", url: "http://a")
              B @join__graph(name: "b", url: "http://b")
            }

            scalar link__Import

            enum link__Purpose {
              """
              `SECURITY` features provide metadata necessary to securely resolve fields.
              """
              SECURITY

              """
              `EXECUTION` features provide metadata necessary for operation execution.
              """
              EXECUTION
            }

            type Query
              @join__type(graph: A)
              @join__type(graph: B)
            {
              q: A @join__field(graph: A)
            }
        "#;

        let schema = Schema::parse(supergraph, "supergraph.graphql").unwrap();
        let ValidFederationSubgraphs { subgraphs } = super::extract_subgraphs_from_supergraph(
            &FederationSchema::new(schema).unwrap(),
            Some(true),
        )
        .unwrap();

        assert_eq!(subgraphs.len(), 2);

        let a = subgraphs.get("a").unwrap();
        // JS PORT NOTE: the original tests used the equivalent of `get_type`,
        // so we have to be careful about using `get_union` here.
        assert!(a.schema.schema().get_union("A").is_some());
        assert!(a.schema.schema().get_object("B").is_some());
        assert!(a.schema.schema().get_object("C").is_some());
        assert!(a.schema.schema().get_object("D").is_some());

        let b = subgraphs.get("b").unwrap();
        assert!(b.schema.schema().get_union("A").is_none());
        assert!(b.schema.schema().get_object("B").is_none());
        assert!(b.schema.schema().get_object("C").is_none());
        assert!(b.schema.schema().get_object("D").is_some());
    }

    #[test]
    fn preserves_default_values_of_input_object_fields() {
        let supergraph = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            {
              query: Query
            }

            directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            input Input
              @join__type(graph: SERVICE)
            {
              a: Int! = 1234
            }

            scalar join__FieldSet

            enum join__Graph {
              SERVICE @join__graph(name: "service", url: "")
            }

            scalar link__Import

            enum link__Purpose {
              """
              `SECURITY` features provide metadata necessary to securely resolve fields.
              """
              SECURITY

              """
              `EXECUTION` features provide metadata necessary for operation execution.
              """
              EXECUTION
            }

            type Query
              @join__type(graph: SERVICE)
            {
              field(input: Input!): String
            }
        "#;

        let schema = Schema::parse(supergraph, "supergraph.graphql").unwrap();
        let ValidFederationSubgraphs { subgraphs } = super::extract_subgraphs_from_supergraph(
            &FederationSchema::new(schema).unwrap(),
            Some(true),
        )
        .unwrap();

        assert_eq!(subgraphs.len(), 1);
        let subgraph = subgraphs.get("service").unwrap();
        let input_type = subgraph.schema.schema().get_input_object("Input").unwrap();
        let input_field_a = input_type
            .fields
            .iter()
            .find(|(name, _)| name == &&name!("a"))
            .unwrap();
        assert_eq!(
            input_field_a.1.default_value.as_ref().unwrap().to_i32(),
            Some(1234)
        );
    }

    // JS PORT NOTE: the "throw meaningful error for invalid federation directive fieldSet"
    // test checked an error condition that can appear only in a Federation 1 supergraph.

    // JS PORT NOTE: the "throw meaningful error for type erased from supergraph due to extending an entity without a key"
    // test checked an error condition that can appear only in a Federation 1 supergraph.

    #[test]
    fn types_that_are_empty_because_of_overridden_fields_are_erased() {
        let supergraph = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
              @link(url: "https://specs.apollo.dev/tag/v0.3")
            {
              query: Query
            }

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA
            input Input
              @join__type(graph: B)
            {
              a: Int! = 1234
            }

            scalar join__FieldSet

            enum join__Graph {
              A @join__graph(name: "a", url: "")
              B @join__graph(name: "b", url: "")
            }

            scalar link__Import

            enum link__Purpose {
              """
              `SECURITY` features provide metadata necessary to securely resolve fields.
              """
              SECURITY

              """
              `EXECUTION` features provide metadata necessary for operation execution.
              """
              EXECUTION
            }

            type Query
              @join__type(graph: A)
            {
              field: String
            }

            type User
              @join__type(graph: A)
              @join__type(graph: B)
            {
              foo: String @join__field(graph: A, override: "b")

              bar: String @join__field(graph: A)

              baz: String @join__field(graph: A)
            }
      "#;

        let schema = Schema::parse(supergraph, "supergraph.graphql").unwrap();
        let ValidFederationSubgraphs { subgraphs } = super::extract_subgraphs_from_supergraph(
            &FederationSchema::new(schema).unwrap(),
            Some(true),
        )
        .unwrap();

        let subgraph = subgraphs.get("a").unwrap();
        let user_type = subgraph.schema.schema().get_object("User");
        assert!(user_type.is_some());

        let subgraph = subgraphs.get("b").unwrap();
        let user_type = subgraph.schema.schema().get_object("User");
        assert!(user_type.is_none());
    }

    #[test]
    fn test_join_directives() {
        let supergraph = r###"schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
                @join__directive(graphs: [SUBGRAPH], name: "link", args: {url: "https://specs.apollo.dev/hello/v0.1", import: ["@hello"]})
            {
                query: Query
            }

            directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            input join__ContextArgument {
                name: String!
                type: String!
                context: String!
                selection: join__FieldValue!
            }

            scalar join__DirectiveArguments

            scalar join__FieldSet

            scalar join__FieldValue

            enum join__Graph {
                SUBGRAPH @join__graph(name: "subgraph", url: "none")
            }

            scalar link__Import

            enum link__Purpose {
                """
                `SECURITY` features provide metadata necessary to securely resolve fields.
                """
                SECURITY

                """
                `EXECUTION` features provide metadata necessary for operation execution.
                """
                EXECUTION
            }

            type Query
                @join__type(graph: SUBGRAPH)
            {
                f: String
            }
        "###;

        let schema = Schema::parse(supergraph, "supergraph.graphql").unwrap();
        let ValidFederationSubgraphs { subgraphs } = super::extract_subgraphs_from_supergraph(
            &FederationSchema::new(schema).unwrap(),
            Some(true),
        )
        .unwrap();

        let subgraph = subgraphs.get("subgraph").unwrap();
        assert_snapshot!(subgraph.schema.schema().schema_definition.directives, @r###" @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.9") @link(url: "https://specs.apollo.dev/hello/v0.1", import: ["@hello"])"###);
    }
}
