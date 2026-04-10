use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use carryover::carryover_directives;
use indexmap::IndexMap;
use itertools::Itertools;
use multimap::MultiMap;
use shape::ShapeCase;

use crate::ApiSchemaOptions;
use crate::Supergraph;
use crate::ValidFederationSubgraph;
use crate::connectors::ConnectSpec;
use crate::connectors::Connector;
use crate::error::FederationError;
use crate::link::join_spec_definition::JOIN_CONNECTED_SELECTION_ARGUMENT_NAME;
use crate::merge::merge_subgraphs;
use crate::schema::FederationSchema;
use crate::subgraph::Subgraph;
use crate::subgraph::ValidSubgraph;

mod carryover;
pub(crate) mod visitors;
use visitors::filter_directives;

use crate::connectors::spec::ConnectLink;

pub struct Connectors {
    pub by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
    pub labels_by_service_name: Arc<IndexMap<Arc<str>, String>>,
    pub source_config_keys: Arc<HashSet<String>>,
}

/// The result of a supergraph expansion of connect-aware subgraphs
pub enum ExpansionResult {
    /// The supergraph had some subgraphs that were expanded
    Expanded {
        raw_sdl: String,
        api_schema: Box<Valid<Schema>>,
        connectors: Connectors,
    },

    /// The supergraph contained no connect directives and was unchanged.
    Unchanged,
}

/// Expand a schema with connector directives into unique subgraphs per directive
///
/// Until we have a source-aware query planner, work with connectors will need to interface
/// with standard query planning concepts while still enforcing connector-specific rules. To do so,
/// each connector is separated into its own unique subgraph with relevant GraphQL directives to enforce
/// field dependencies and response structures. This allows for satisfiability and validation to piggy-back
/// off of existing functionality in a reproducible way.
pub fn expand_connectors(
    supergraph_str: &str,
    api_schema_options: &ApiSchemaOptions,
) -> Result<ExpansionResult, FederationError> {
    // TODO: Don't rely on finding the URL manually to short out
    let connect_url = ConnectSpec::identity();
    let connect_url = format!("{}/{}/v", connect_url.domain, connect_url.name);
    if !supergraph_str.contains(&connect_url) {
        return Ok(ExpansionResult::Unchanged);
    }

    let supergraph = Supergraph::new_with_router_specs(supergraph_str)?;
    let api_schema = supergraph.to_api_schema(api_schema_options.clone())?;

    let all_subgraphs: Vec<_> = supergraph.extract_subgraphs()?.into_iter().collect();
    let subgraph_connect_directive_names: HashMap<String, [Name; 2]> = all_subgraphs
        .iter()
        .flat_map(|(_, sub)| {
            let Some(Ok(link)) = ConnectLink::new(sub.schema.schema()) else {
                return None;
            };
            Some((
                sub.name.clone(),
                [link.connect_directive_name, link.source_directive_name],
            ))
        })
        .collect::<HashMap<_, _>>();

    let (connect_subgraphs, graphql_subgraphs): (Vec<_>, Vec<_>) = all_subgraphs
        .into_iter()
        .partition_map(|(_, sub)| match ConnectLink::new(sub.schema.schema()) {
            Some(Ok(link)) if contains_connectors(&link, &sub) => either::Either::Left((link, sub)),
            _ => either::Either::Right(ValidSubgraph::from(sub)),
        });

    // Expand just the connector subgraphs
    let mut expanded_subgraphs = Vec::new();

    for (link, sub) in connect_subgraphs {
        expanded_subgraphs.extend(split_subgraph(&link, sub)?);
    }

    // Merge the subgraphs into one supergraph
    let all_subgraphs = graphql_subgraphs
        .iter()
        .chain(expanded_subgraphs.iter().map(|(_, sub)| sub))
        .collect();
    let new_supergraph = merge_subgraphs(all_subgraphs).map_err(|e| {
        FederationError::internal(format!("could not merge expanded subgraphs: {e:?}"))
    })?;

    let subgraph_name_replacements = expanded_subgraphs
        .iter()
        .map(|(connector, _)| {
            (
                connector.id.subgraph_name.as_str(),
                connector.id.synthetic_name(),
            )
        })
        .collect::<MultiMap<_, _>>();

    let mut new_supergraph = FederationSchema::new(new_supergraph.schema.into_inner())?;
    carryover_directives(
        &supergraph.schema,
        &mut new_supergraph,
        &subgraph_name_replacements,
        subgraph_connect_directive_names,
    )
    .map_err(|e| FederationError::internal(format!("could not carry over directives: {e:?}")))?;

    let connectors_by_service_name: IndexMap<Arc<str>, Connector> = expanded_subgraphs
        .into_iter()
        .map(|(connector, sub)| (sub.name.into(), connector))
        .collect();

    // Add connectedSelection to @join__field for recursive connector types
    add_connected_selections(
        supergraph.schema.schema(),
        &mut new_supergraph,
        &connectors_by_service_name,
    )?;

    let labels_by_service_name = connectors_by_service_name
        .iter()
        .map(|(service_name, connector)| (service_name.clone(), connector.label.0.clone()))
        .collect();

    let source_config_keys = connectors_by_service_name
        .iter()
        .map(|(_, connector)| connector.source_config_key())
        .collect();

    Ok(ExpansionResult::Expanded {
        raw_sdl: new_supergraph.schema().serialize().to_string(),
        api_schema: Box::new(api_schema.schema().clone()),
        connectors: Connectors {
            by_service_name: Arc::new(connectors_by_service_name),
            labels_by_service_name: Arc::new(labels_by_service_name),
            source_config_keys: Arc::new(source_config_keys),
        },
    })
}

fn contains_connectors(link: &ConnectLink, subgraph: &ValidFederationSubgraph) -> bool {
    subgraph
        .schema
        .get_directive_definitions()
        .any(|directive| {
            directive.directive_name == link.connect_directive_name
                || directive.directive_name == link.source_directive_name
        })
}

/// Add `connectedSelection` to `@join__field` directives for entity resolver connectors.
///
/// For any field-level entity resolver connector (not on Query/Mutation root types),
/// this function annotates the corresponding `@join__field` directive with the connector's
/// selection field names so the query graph builder can create restricted copy nodes.
/// This handles both direct cycles (User.friends: [User]) and indirect cycles
/// (Track.modules: [Module], Module.track: Track).
fn add_connected_selections(
    original_schema: &Valid<Schema>,
    supergraph: &mut FederationSchema,
    connectors: &IndexMap<Arc<str>, Connector>,
) -> Result<(), FederationError> {
    // Build service_name -> join__Graph enum value name mapping
    let service_name_to_enum_value = build_service_name_to_enum_value(supergraph.schema())?;

    // Collect (type_name, field_name, enum_value, connected_selection_str) tuples
    let mut annotations: Vec<(Name, Name, Name, String)> = Vec::new();

    for (service_name, connector) in connectors {
        // Only field-level connectors can be recursive
        let Some(parent_type_name) = connector.id.directive.parent_type_name() else {
            continue;
        };

        // Only non-root entity resolvers need connectedSelection.
        // Root connectors (Query/Mutation) and non-entity-resolver connectors are skipped.
        if connector.entity_resolver.is_none()
            || connector.id.directive.on_root_type(original_schema)
        {
            continue;
        }

        let Some(graph_enum_value) = service_name_to_enum_value.get(service_name.as_ref()) else {
            continue;
        };

        // The connector's selection field names are the connected selection
        let shape = connector.selection.shape();
        if let Some(selection_str) = extract_shape_as_field_set(&shape) {
            annotations.push((
                parent_type_name.clone(),
                connector
                    .id
                    .directive
                    .field_definition(original_schema)
                    .map(|f| f.name.clone())
                    .ok_or_else(|| {
                        FederationError::internal("field definition not found for connector")
                    })?,
                graph_enum_value.clone(),
                selection_str,
            ));
        }
    }

    if annotations.is_empty() {
        return Ok(());
    }

    // Apply annotations to the supergraph schema
    let schema = supergraph.schema_mut();

    // Ensure the @join__field directive definition declares `connectedSelection`.
    // The expansion may produce join/v0.5 SDL which lacks this argument, but we
    // need it so the expanded supergraph passes GraphQL validation.
    if let Some(join_field_def) = schema.directive_definitions.get_mut(&name!("join__field")) {
        let already_has_arg = join_field_def
            .arguments
            .iter()
            .any(|a| a.name == JOIN_CONNECTED_SELECTION_ARGUMENT_NAME);
        if !already_has_arg {
            use apollo_compiler::ast::InputValueDefinition;
            use apollo_compiler::ast::Type;
            let join_field_def = join_field_def.make_mut();
            join_field_def
                .arguments
                .push(Node::new(InputValueDefinition {
                    description: None,
                    name: JOIN_CONNECTED_SELECTION_ARGUMENT_NAME,
                    ty: Node::new(Type::Named(name!("join__FieldSet"))),
                    default_value: None,
                    directives: Default::default(),
                }));
        }
    }

    for (type_name, field_name, enum_value, selection_str) in annotations {
        let Some(ExtendedType::Object(obj)) = schema.types.get_mut(&type_name) else {
            continue;
        };
        let obj = obj.make_mut();
        let Some(field) = obj.fields.get_mut(&field_name) else {
            continue;
        };
        let field = field.make_mut();

        // Find the @join__field directive matching this graph enum value
        for directive in field.directives.0.iter_mut() {
            if directive.name != name!("join__field") {
                continue;
            }
            let has_matching_graph = directive.arguments.iter().any(|arg| {
                arg.name == name!("graph")
                    && matches!(arg.value.as_ref(), Value::Enum(v) if *v == enum_value)
            });
            if has_matching_graph {
                // Add connectedSelection argument
                let directive = directive.make_mut();
                directive.arguments.push(Node::new(Argument {
                    name: JOIN_CONNECTED_SELECTION_ARGUMENT_NAME,
                    value: Node::new(Value::String(selection_str.clone())),
                }));
                break;
            }
        }
    }

    Ok(())
}

/// Build a mapping from service name (e.g. "connectors_Query_user_0") to the
/// join__Graph enum value name (e.g. "CONNECTORS_QUERY_USER_0").
fn build_service_name_to_enum_value(
    schema: &Schema,
) -> Result<HashMap<String, Name>, FederationError> {
    let mut map = HashMap::default();

    let Some(ExtendedType::Enum(join_graph_enum)) = schema.types.get("join__Graph") else {
        return Ok(map);
    };

    for (enum_value_name, enum_value_def) in &join_graph_enum.values {
        // Look for @join__graph(name: "service_name") on this enum value
        for directive in enum_value_def.directives.get_all(&name!("join__graph")) {
            for arg in &directive.arguments {
                if arg.name == name!("name")
                    && let Value::String(service_name) = arg.value.as_ref()
                {
                    map.insert(service_name.clone(), enum_value_name.clone());
                }
            }
        }
    }

    Ok(map)
}

/// Extract a FieldSet string from a Shape, recursing into composite fields.
///
/// For a shape representing `{ id, name, friends: [{ id, name }] }`, this
/// produces `"id name friends { id name }"`. This ensures the restricted copy
/// node in the query graph accurately reflects which fields (including nested
/// ones) the connector's HTTP endpoint returns, avoiding unnecessary entity
/// resolution fetches.
fn extract_shape_as_field_set(shape: &shape::Shape) -> Option<String> {
    match shape.case() {
        ShapeCase::Object { fields, .. } => {
            let parts: Vec<String> = fields
                .iter()
                .filter(|(k, _)| k.as_str() != "__typename")
                .map(|(k, v)| match extract_shape_as_field_set(v) {
                    Some(nested) => format!("{k} {{ {nested} }}"),
                    None => k.to_string(),
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        // Handle arrays: extract from the tail (element type)
        ShapeCase::Array { tail, .. } => extract_shape_as_field_set(tail),
        // Handle One (union): try each member
        ShapeCase::One(shapes) => {
            for member in shapes.iter() {
                if let Some(field_set) = extract_shape_as_field_set(member) {
                    return Some(field_set);
                }
            }
            None
        }
        _ => None,
    }
}

/// Split up a subgraph so that each connector directive becomes its own subgraph.
///
/// Subgraphs passed to this function should contain connector directives.
fn split_subgraph(
    link: &ConnectLink,
    subgraph: ValidFederationSubgraph,
) -> Result<Vec<(Connector, ValidSubgraph)>, FederationError> {
    let connector_map = Connector::from_schema(subgraph.schema.schema(), &subgraph.name)?;

    // Fork based on ConnectSpec version:
    // - v0.1/v0.2/v0.3: Use legacy visitor-based expansion (frozen for compatibility)
    // - v0.4+: Use shape-driven expansion (actively maintained)
    if link.spec < ConnectSpec::V0_4 {
        // Legacy path for v0.1/v0.2/v0.3 compatibility
        let expander = helpers::LegacyExpander::new(link, &subgraph);
        connector_map
            .into_iter()
            .map(|connector| {
                // Build a subgraph using only the necessary fields from the directive
                let schema = expander.expand(&connector)?;
                let subgraph = Subgraph::new(
                    connector.id.synthetic_name().as_str(),
                    &subgraph.url,
                    &schema.schema().serialize().to_string(),
                )?;

                // We only validate during debug builds since we should realistically only generate valid schemas
                // for these subgraphs.
                #[cfg(debug_assertions)]
                let schema = subgraph.schema.validate()?;
                #[cfg(not(debug_assertions))]
                let schema = Valid::assume_valid(subgraph.schema);

                Ok((
                    connector,
                    ValidSubgraph {
                        name: subgraph.name,
                        url: subgraph.url,
                        schema,
                    },
                ))
            })
            .try_collect()
    } else {
        // Modern path for v0.4+: shape-driven expansion
        let expander = helpers::Expander::new(link, &subgraph);
        connector_map
            .into_iter()
            .map(|connector| {
                // Build a subgraph using only the necessary fields from the directive
                let schema = expander.expand(&connector)?;
                let subgraph = Subgraph::new(
                    connector.id.synthetic_name().as_str(),
                    &subgraph.url,
                    &schema.schema().serialize().to_string(),
                )?;

                // We only validate during debug builds since we should realistically only generate valid schemas
                // for these subgraphs.
                #[cfg(debug_assertions)]
                let schema = subgraph.schema.validate()?;
                #[cfg(not(debug_assertions))]
                let schema = Valid::assume_valid(subgraph.schema);

                Ok((
                    connector,
                    ValidSubgraph {
                        name: subgraph.name,
                        url: subgraph.url,
                        schema,
                    },
                ))
            })
            .try_collect()
    }
}

mod helpers {
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::ast;
    use apollo_compiler::ast::Argument;
    use apollo_compiler::ast::Directive;
    use apollo_compiler::ast::FieldDefinition;
    use apollo_compiler::ast::InputValueDefinition;
    use apollo_compiler::ast::Value;
    use apollo_compiler::name;
    use apollo_compiler::schema::Component;
    use apollo_compiler::schema::ComponentName;
    use apollo_compiler::schema::ComponentOrigin;
    use apollo_compiler::schema::DirectiveList;
    use apollo_compiler::schema::EnumType;
    use apollo_compiler::schema::ObjectType;
    use apollo_compiler::schema::ScalarType;
    use apollo_compiler::ty;
    use indexmap::IndexMap;
    use indexmap::IndexSet;

    use super::filter_directives;
    use super::visitors::GroupVisitor;
    use super::visitors::SchemaVisitor;
    use super::visitors::selection::walk_type_with_shape;
    use super::visitors::try_insert;
    use super::visitors::try_pre_insert;
    use crate::ValidFederationSubgraph;
    use crate::connectors::ConnectSpec;
    use crate::connectors::Connector;
    use crate::connectors::EntityResolver;
    use crate::connectors::JSONSelection;
    use crate::connectors::id::ConnectedElement;
    use crate::connectors::spec::ConnectLink;
    use crate::error::FederationError;
    use crate::internal_error;
    use crate::link::spec::Identity;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::InterfaceFieldDefinitionPosition;
    use crate::schema::position::ObjectFieldDefinitionPosition;
    use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;
    use crate::schema::position::SchemaRootDefinitionPosition;
    use crate::schema::position::TypeDefinitionPosition;
    use crate::subgraph::spec::EXTERNAL_DIRECTIVE_NAME;
    use crate::subgraph::spec::INTF_OBJECT_DIRECTIVE_NAME;
    use crate::subgraph::spec::KEY_DIRECTIVE_NAME;
    use crate::subgraph::spec::REQUIRES_DIRECTIVE_NAME;
    use crate::supergraph::new_empty_fed_2_subgraph_schema;

    /// Create the appropriate field position for a type (object or interface) and
    /// insert the field if it doesn't already exist in the schema.
    fn insert_field_if_missing(
        type_pos: &TypeDefinitionPosition,
        field_name: Name,
        field_def: Component<FieldDefinition>,
        to_schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let pos: ObjectOrInterfaceFieldDefinitionPosition = match type_pos {
            TypeDefinitionPosition::Object(obj) => ObjectFieldDefinitionPosition {
                type_name: obj.type_name.clone(),
                field_name,
            }
            .into(),
            TypeDefinitionPosition::Interface(iface) => InterfaceFieldDefinitionPosition {
                type_name: iface.type_name.clone(),
                field_name,
            }
            .into(),
            other => {
                return Err(FederationError::internal(format!(
                    "expected object or interface type, found {}",
                    other.type_name()
                )));
            }
        };

        if pos.get(to_schema.schema()).is_err() {
            pos.insert(to_schema, field_def)?;
        }
        Ok(())
    }

    /// A helper struct for expanding a subgraph into one per connect directive.
    /// This is the shape-driven version for connect/v0.4+.
    pub(super) struct Expander<'a> {
        /// The name of the @key directive, as known in the subgraph
        key_name: Name,

        /// The name of the @interfaceObject directive, as known in the subgraph
        interface_object_name: Name,

        /// The original schema that contains connect directives
        original_schema: &'a ValidFederationSchema,

        /// A list of directives to exclude when copying over types from the
        /// original schema.
        directive_deny_list: IndexSet<Name>,

        /// The ConnectSpec version from the @link directive
        spec: ConnectSpec,
    }

    impl<'a> Expander<'a> {
        pub(super) fn new(link: &ConnectLink, subgraph: &'a ValidFederationSubgraph) -> Self {
            let key_name = subgraph
                .schema
                .metadata()
                .and_then(|m| m.for_identity(&Identity::federation_identity()))
                .map_or(KEY_DIRECTIVE_NAME, |f| {
                    f.directive_name_in_schema(&KEY_DIRECTIVE_NAME)
                });
            let interface_object_name = subgraph
                .schema
                .metadata()
                .and_then(|m| m.for_identity(&Identity::federation_identity()))
                .map_or(INTF_OBJECT_DIRECTIVE_NAME, |f| {
                    f.directive_name_in_schema(&INTF_OBJECT_DIRECTIVE_NAME)
                });
            let extra_excluded = [EXTERNAL_DIRECTIVE_NAME, REQUIRES_DIRECTIVE_NAME]
                .into_iter()
                .map(|d| {
                    subgraph
                        .schema
                        .metadata()
                        .and_then(|m| m.for_identity(&Identity::federation_identity()))
                        .map(|f| f.directive_name_in_schema(&d))
                        .unwrap_or(d)
                });
            let directive_deny_list = IndexSet::from_iter(extra_excluded.chain([
                key_name.clone(),
                link.connect_directive_name.clone(),
                link.source_directive_name.clone(),
            ]));

            Self {
                key_name,
                interface_object_name,
                original_schema: &subgraph.schema,
                directive_deny_list,
                spec: link.spec,
            }
        }

        /// Build an expanded subgraph for the supplied connector
        pub(super) fn expand(
            &self,
            connector: &Connector,
        ) -> Result<FederationSchema, FederationError> {
            let mut schema = new_empty_fed_2_subgraph_schema()?;
            let query_alias = self
                .original_schema
                .schema()
                .schema_definition
                .query
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or(name!("Query"));
            let mutation_alias = self
                .original_schema
                .schema()
                .schema_definition
                .mutation
                .as_ref()
                .map(|m| m.name.clone());

            let element = connector
                .id
                .directive
                .element(self.original_schema.schema())
                .map_err(|_| {
                    FederationError::internal("Elements for connector position not found")
                })?;

            match element {
                ConnectedElement::Field {
                    field_def,
                    parent_type,
                    ..
                } => {
                    let field_type = self
                        .original_schema
                        .get_type(field_def.ty.inner_named_type().clone())?;

                    // We'll need to make sure that we always process the inputs first, since they need to be present
                    // before any dependent types
                    self.process_inputs(&mut schema, &field_def.arguments)?;

                    // InputObjects are never valid as return types (input-only in GraphQL)
                    if let TypeDefinitionPosition::InputObject(input) = &field_type {
                        return Err(FederationError::internal(format!(
                            "input object type {} cannot be used as a @connect field return type",
                            input.type_name
                        )));
                    }

                    if self.spec < ConnectSpec::V0_4 {
                        match &field_type {
                            TypeDefinitionPosition::Object(_object) => {
                                if connector.selection.is_empty() {
                                    return Err(FederationError::internal(
                                        "empty selections are not allowed",
                                    ));
                                }
                            }

                            TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_) => {
                                // Now handled below, within walk_type_with_shape:
                                // self.insert_custom_leaf(&mut schema, &field_type)?;
                            }

                            TypeDefinitionPosition::Interface(interface) => {
                                return Err(FederationError::internal(format!(
                                    "interface type {} not supported in connect/v0.3 and earlier; use @link(url: \"https://specs.apollo.dev/connect/v0.4\") to enable interface support",
                                    interface.type_name
                                )));
                            }
                            TypeDefinitionPosition::Union(union) => {
                                return Err(FederationError::internal(format!(
                                    "union type {} not supported in connect/v0.3 and earlier; use @link(url: \"https://specs.apollo.dev/connect/v0.4\") to enable union support",
                                    union.type_name
                                )));
                            }
                            TypeDefinitionPosition::InputObject(_) => {
                                // Already checked above - unreachable
                            }
                        }
                    }

                    walk_type_with_shape(
                        &field_type,
                        &connector.selection.shape(),
                        self.original_schema,
                        &mut schema,
                        &self.directive_deny_list,
                        self.spec,
                    )?;

                    // Add the root type for this connector, optionally inserting a dummy query root
                    // if the connector is not defined within a field on a Query (since a subgraph is invalid
                    // without at least a root-level Query)

                    let parent_pos = ObjectTypeDefinitionPosition {
                        type_name: parent_type.name().clone(),
                    };

                    self.insert_object_and_field(&mut schema, &parent_pos, field_def)?;
                    self.ensure_query_root_type(
                        &mut schema,
                        &query_alias,
                        Some(parent_type.name()),
                    )?;
                    if let Some(mutation_alias) = mutation_alias {
                        self.ensure_mutation_root_type(
                            &mut schema,
                            &mutation_alias,
                            parent_type.name(),
                        )?;
                    }

                    // Process any outputs needed by the connector
                    self.process_outputs(
                        &mut schema,
                        connector,
                        parent_type.name().clone(),
                        field_def.ty.inner_named_type().clone(),
                    )?;
                }
                ConnectedElement::Type { type_ref } => {
                    let type_def_pos =
                        TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
                            type_name: type_ref.name().clone(),
                        });
                    let shape = connector.selection.shape();
                    walk_type_with_shape(
                        &type_def_pos,
                        &shape,
                        self.original_schema,
                        &mut schema,
                        &self.directive_deny_list,
                        self.spec,
                    )?;

                    // we need a Query root field to be valid
                    self.ensure_query_root_type(&mut schema, &query_alias, None)?;

                    // Process any outputs needed by the connector
                    self.process_outputs(
                        &mut schema,
                        connector,
                        type_ref.name().clone(),
                        type_ref.name().clone(),
                    )?;
                }
            }

            Ok(schema)
        }

        /// Process all input types
        ///
        /// Inputs can include leaf types as well as custom inputs.
        fn process_inputs(
            &self,
            to_schema: &mut FederationSchema,
            arguments: &[Node<InputValueDefinition>],
        ) -> Result<(), FederationError> {
            // All inputs to a connector's field need to be carried over in order to always generate
            // valid subgraphs
            for arg in arguments {
                let arg_type_name = arg.ty.inner_named_type();
                let arg_type = self.original_schema.get_type(arg_type_name.clone())?;
                let arg_extended_type = arg_type.get(self.original_schema.schema())?;

                // If the input type isn't built in, then we need to carry it over, making sure to only walk
                // if we have a complex input since leaf types can just be copied over.
                if !arg_extended_type.is_built_in() {
                    match arg_type {
                        TypeDefinitionPosition::InputObject(input) => {
                            SchemaVisitor::new(
                                self.original_schema,
                                to_schema,
                                &self.directive_deny_list,
                            )
                            .walk(input)?;
                        }
                        other => self.insert_custom_leaf(to_schema, &other)?,
                    };
                }
            }

            Ok(())
        }

        // Process outputs needed by a connector
        //
        // By the time this method is called, all dependent types should exist for a connector,
        // including its direct inputs. Since each connector could select only a subset of its output
        // type, this method carries over each output type as seen by the selection defined on the connector.
        fn process_outputs(
            &self,
            to_schema: &mut FederationSchema,
            connector: &Connector,
            parent_type_name: Name,
            output_type_name: Name,
        ) -> Result<(), FederationError> {
            let resolvable_key = connector
                .resolvable_key(self.original_schema.schema())
                .map_err(|_| FederationError::internal("error creating resolvable key"))?;

            let Some(resolvable_key) = resolvable_key else {
                return self.copy_interface_object_keys(output_type_name, to_schema);
            };

            let parent_type = self.original_schema.get_type(parent_type_name)?;
            let output_type = to_schema.get_type(output_type_name)?;
            let key_for_type = match &connector.entity_resolver {
                Some(EntityResolver::Explicit) => output_type,
                _ => parent_type,
            };

            let parsed = JSONSelection::parse_with_spec(
                &resolvable_key.serialize().no_indent().to_string(),
                connector.spec,
            )
            .map_err(|e| FederationError::internal(format!("error parsing key: {e}")))?;

            // This adds child types for all key fields
            walk_type_with_shape(
                &key_for_type,
                &parsed.shape(),
                self.original_schema,
                to_schema,
                &self.directive_deny_list,
                self.spec,
            )?;

            // This actually adds the key fields if necessary, which is only
            // when depending on sibling fields.
            if let Some(sub) = parsed.next_subselection() {
                for named in sub.selections_iter() {
                    for field_name in named.names() {
                        let field_def = self
                            .original_schema
                            .schema()
                            .type_field(key_for_type.type_name(), field_name)
                            .map_err(|_| {
                                FederationError::internal(format!(
                                    "field {} not found on type {}",
                                    field_name,
                                    key_for_type.type_name()
                                ))
                            })?;

                        insert_field_if_missing(
                            &key_for_type,
                            Name::new(field_name)?,
                            Component::new(FieldDefinition {
                                description: field_def.description.clone(),
                                name: field_def.name.clone(),
                                arguments: field_def.arguments.clone(),
                                ty: field_def.ty.clone(),
                                directives: filter_directives(
                                    &self.directive_deny_list,
                                    &field_def.directives,
                                ),
                            }),
                            to_schema,
                        )?;
                    }
                }
            };

            // If we have marked keys as being necessary for this output type, add them as an `@key`
            // directive now.
            let key_directive = Directive {
                name: self.key_name.clone(),
                arguments: vec![Node::new(Argument {
                    name: name!("fields"),
                    value: Node::new(Value::String(
                        resolvable_key.serialize().no_indent().to_string(),
                    )),
                })],
            };

            match &key_for_type {
                TypeDefinitionPosition::Object(o) => {
                    o.insert_directive(to_schema, Component::new(key_directive))?;
                }
                TypeDefinitionPosition::Interface(i) => {
                    i.insert_directive(to_schema, Component::new(key_directive.clone()))?;
                    // Federation requires implementing types to also have the interface's @key
                    if let Some(implementers) = self
                        .original_schema
                        .schema()
                        .implementers_map()
                        .get(&i.type_name)
                    {
                        for implementer in &implementers.objects {
                            let obj_pos = ObjectTypeDefinitionPosition {
                                type_name: implementer.clone(),
                            };
                            obj_pos.insert_directive(
                                to_schema,
                                Component::new(key_directive.clone()),
                            )?;
                        }
                    }
                }
                _ => {
                    return Err(FederationError::internal(
                        "keys cannot be added to scalars, unions, enums, or input objects",
                    ));
                }
            }

            Ok(())
        }

        /// If the type has @interfaceObject and it doesn't have a key at this point
        /// we'll need to add a key — this is a requirement for using @interfaceObject.
        fn copy_interface_object_keys(
            &self,
            type_name: Name,
            to_schema: &mut FederationSchema,
        ) -> Result<(), FederationError> {
            let Some(original_output_type) = self.original_schema.schema().get_object(&type_name)
            else {
                return Ok(());
            };

            let is_interface_object = original_output_type
                .directives
                .iter()
                .any(|d| d.name == self.interface_object_name);

            let pos = ObjectTypeDefinitionPosition {
                type_name: original_output_type.name.clone(),
            };

            for key in original_output_type
                .directives
                .iter()
                .filter(|d| d.name == self.key_name)
            {
                let key_fields = key
                    .argument_by_name("fields", self.original_schema.schema())
                    .map_err(|_| internal_error!("@key(fields:) argument missing"))?;

                let mut arguments = vec![Node::new(Argument {
                    name: name!("fields"),
                    value: key_fields.clone(),
                })];

                if is_interface_object {
                    arguments.push(Node::new(Argument {
                        name: name!("resolvable"),
                        value: Node::new(Value::Boolean(false)),
                    }));
                }

                let key = Directive {
                    name: key.name.clone(),
                    arguments,
                };
                pos.insert_directive(to_schema, Component::new(key))?;
            }

            Ok(())
        }

        /// Inserts a custom leaf type into the schema
        fn insert_custom_leaf(
            &self,
            to_schema: &mut FederationSchema,
            r#type: &TypeDefinitionPosition,
        ) -> Result<(), FederationError> {
            match r#type {
                TypeDefinitionPosition::Scalar(scalar) => {
                    let def = scalar.get(self.original_schema.schema())?;
                    let def = ScalarType {
                        description: def.description.clone(),
                        name: def.name.clone(),
                        directives: filter_directives(&self.directive_deny_list, &def.directives),
                    };

                    try_pre_insert!(to_schema, scalar)?;
                    try_insert!(to_schema, scalar, Node::new(def))
                }
                TypeDefinitionPosition::Enum(r#enum) => {
                    let def = r#enum.get(self.original_schema.schema())?;
                    let def = EnumType {
                        description: def.description.clone(),
                        name: def.name.clone(),
                        directives: filter_directives(&self.directive_deny_list, &def.directives),
                        values: def.values.clone(),
                    };

                    try_pre_insert!(to_schema, r#enum)?;
                    try_insert!(to_schema, r#enum, Node::new(def))
                }

                other => Err(FederationError::internal(format!(
                    "expected a leaf, found: {}",
                    other.type_name(),
                ))),
            }
        }

        /// Insert the parent type and field definition for a connector
        fn insert_object_and_field(
            &self,
            to_schema: &mut FederationSchema,
            field_parent: &ObjectTypeDefinitionPosition,
            field: impl AsRef<FieldDefinition>,
        ) -> Result<(), FederationError> {
            let original = field.as_ref();

            let parent_type = field_parent.get(self.original_schema.schema())?;

            try_pre_insert!(to_schema, field_parent)?;
            let field_def = FieldDefinition {
                description: original.description.clone(),
                name: original.name.clone(),
                arguments: original.arguments.clone(),
                ty: original.ty.clone(),
                directives: filter_directives(&self.directive_deny_list, &original.directives),
            };
            try_insert!(
                to_schema,
                field_parent,
                Node::new(ObjectType {
                    description: parent_type.description.clone(),
                    name: parent_type.name.clone(),
                    implements_interfaces: parent_type.implements_interfaces.clone(),
                    directives: filter_directives(
                        &self.directive_deny_list,
                        &parent_type.directives,
                    ),
                    fields: Default::default()
                })
            )?;

            let pos = ObjectFieldDefinitionPosition {
                type_name: parent_type.name.clone(),
                field_name: field_def.name.clone(),
            };

            pos.insert(to_schema, field_def.into())?;

            Ok(())
        }

        /// Insert a query root type for a connect field
        fn ensure_query_root_type(
            &self,
            to_schema: &mut FederationSchema,
            query_alias: &Name,
            parent_type_name: Option<&Name>,
        ) -> Result<(), FederationError> {
            if parent_type_name.is_none_or(|name| name != query_alias) {
                let query = ObjectTypeDefinitionPosition {
                    type_name: query_alias.clone(),
                };

                let dummy_field_def = FieldDefinition {
                    description: None,
                    name: name!("_"),
                    arguments: Vec::new(),
                    ty: ty!(ID),
                    directives: ast::DirectiveList(vec![Node::new(Directive {
                        name: name!("federation__inaccessible"),
                        arguments: Vec::new(),
                    })]),
                };

                query.pre_insert(to_schema)?;
                query.insert(
                    to_schema,
                    Node::new(ObjectType {
                        description: None,
                        name: query_alias.clone(),
                        implements_interfaces: IndexSet::with_hasher(Default::default()),
                        directives: DirectiveList::new(),
                        fields: IndexMap::from_iter([(
                            dummy_field_def.name.clone(),
                            Component::new(dummy_field_def),
                        )]),
                    }),
                )?;
            }

            SchemaRootDefinitionPosition {
                root_kind: SchemaRootDefinitionKind::Query,
            }
            .insert(
                to_schema,
                ComponentName {
                    origin: ComponentOrigin::Definition,
                    name: query_alias.clone(),
                },
            )?;

            Ok(())
        }

        /// Adds the mutation root type to the schema definition if necessary
        fn ensure_mutation_root_type(
            &self,
            to_schema: &mut FederationSchema,
            mutation_alias: &Name,
            parent_type_name: &Name,
        ) -> Result<(), FederationError> {
            if mutation_alias == parent_type_name
                && to_schema.get_type(mutation_alias.clone()).is_ok()
            {
                let mutation_root = SchemaRootDefinitionPosition {
                    root_kind: SchemaRootDefinitionKind::Mutation,
                };
                mutation_root.insert(
                    to_schema,
                    ComponentName {
                        origin: ComponentOrigin::Definition,
                        name: mutation_alias.clone(),
                    },
                )?;
            }

            Ok(())
        }
    }

    /// A helper struct for expanding a subgraph into one per connect directive.
    /// This is the legacy visitor-based version for connect/v0.1-v0.3.
    pub(super) struct LegacyExpander<'a> {
        /// The name of the @key directive, as known in the subgraph
        key_name: Name,

        /// The name of the @interfaceObject directive, as known in the subgraph
        interface_object_name: Name,

        /// The original schema that contains connect directives
        original_schema: &'a ValidFederationSchema,

        /// A list of directives to exclude when copying over types from the
        /// original schema.
        directive_deny_list: IndexSet<Name>,
    }

    impl<'a> LegacyExpander<'a> {
        pub(super) fn new(link: &ConnectLink, subgraph: &'a ValidFederationSubgraph) -> Self {
            // When we go to expand all output types, we'll need to make sure that we don't carry over
            // any connect-related directives. The following directives are also special because they
            // influence planning and satisfiability:
            //
            // - @key: derived based on the fields selected
            // - @external: the current approach will only add external fields to the list of keys
            //     if used in the transport. If not used at all, the field marked with this directive
            //     won't even be included in the expanded subgraph, but if it _is_ used then leaving
            //     this directive will result in planning failures.
            // - @requires: the current approach will add required fields to the list of keys for
            //     implicit entities, so it can't stay.
            let key_name = subgraph
                .schema
                .metadata()
                .and_then(|m| m.for_identity(&Identity::federation_identity()))
                .map_or(KEY_DIRECTIVE_NAME, |f| {
                    f.directive_name_in_schema(&KEY_DIRECTIVE_NAME)
                });
            let interface_object_name = subgraph
                .schema
                .metadata()
                .and_then(|m| m.for_identity(&Identity::federation_identity()))
                .map_or(INTF_OBJECT_DIRECTIVE_NAME, |f| {
                    f.directive_name_in_schema(&INTF_OBJECT_DIRECTIVE_NAME)
                });
            let extra_excluded = [EXTERNAL_DIRECTIVE_NAME, REQUIRES_DIRECTIVE_NAME]
                .into_iter()
                .map(|d| {
                    subgraph
                        .schema
                        .metadata()
                        .and_then(|m| m.for_identity(&Identity::federation_identity()))
                        .map(|f| f.directive_name_in_schema(&d))
                        .unwrap_or(d)
                });
            let directive_deny_list = IndexSet::from_iter(extra_excluded.chain([
                key_name.clone(),
                link.connect_directive_name.clone(),
                link.source_directive_name.clone(),
            ]));

            Self {
                key_name,
                interface_object_name,
                original_schema: &subgraph.schema,
                directive_deny_list,
            }
        }

        /// Build an expanded subgraph for the supplied connector
        pub(super) fn expand(
            &self,
            connector: &Connector,
        ) -> Result<FederationSchema, FederationError> {
            let mut schema = new_empty_fed_2_subgraph_schema()?;
            let query_alias = self
                .original_schema
                .schema()
                .schema_definition
                .query
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or(name!("Query"));
            let mutation_alias = self
                .original_schema
                .schema()
                .schema_definition
                .mutation
                .as_ref()
                .map(|m| m.name.clone());

            let element = connector
                .id
                .directive
                .element(self.original_schema.schema())
                .map_err(|_| {
                    FederationError::internal("Elements for connector position not found")
                })?;

            match element {
                ConnectedElement::Field {
                    field_def,
                    parent_type,
                    ..
                } => {
                    let field_type = self
                        .original_schema
                        .get_type(field_def.ty.inner_named_type().clone())?;

                    // We'll need to make sure that we always process the inputs first, since they need to be present
                    // before any dependent types
                    self.process_inputs(&mut schema, &field_def.arguments)?;

                    // Actually process the type annotated with the connector, making sure to walk nested types
                    match field_type {
                        TypeDefinitionPosition::Object(object) => {
                            SchemaVisitor::new(
                                self.original_schema,
                                &mut schema,
                                &self.directive_deny_list,
                            )
                            .walk((
                                object,
                                connector
                                    .selection
                                    .next_subselection()
                                    .cloned()
                                    .ok_or_else(|| {
                                        FederationError::internal(
                                            "empty selections are not allowed",
                                        )
                                    })?,
                            ))?;
                        }

                        TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_) => {
                            self.insert_custom_leaf(&mut schema, &field_type)?;
                        }

                        TypeDefinitionPosition::Interface(interface) => {
                            return Err(FederationError::internal(format!(
                                "connect directives not yet supported on interfaces: found on {}",
                                interface.type_name
                            )));
                        }
                        TypeDefinitionPosition::Union(union) => {
                            return Err(FederationError::internal(format!(
                                "connect directives not yet supported on union: found on {}",
                                union.type_name
                            )));
                        }
                        TypeDefinitionPosition::InputObject(input) => {
                            return Err(FederationError::internal(format!(
                                "connect directives not yet supported on inputs: found on {}",
                                input.type_name
                            )));
                        }
                    };

                    // Add the root type for this connector, optionally inserting a dummy query root
                    // if the connector is not defined within a field on a Query (since a subgraph is invalid
                    // without at least a root-level Query)

                    let parent_pos = ObjectTypeDefinitionPosition {
                        type_name: parent_type.name().clone(),
                    };

                    self.insert_object_and_field(&mut schema, &parent_pos, field_def)?;
                    self.ensure_query_root_type(
                        &mut schema,
                        &query_alias,
                        Some(parent_type.name()),
                    )?;
                    if let Some(mutation_alias) = mutation_alias {
                        self.ensure_mutation_root_type(
                            &mut schema,
                            &mutation_alias,
                            parent_type.name(),
                        )?;
                    }

                    // Process any outputs needed by the connector
                    self.process_outputs(
                        &mut schema,
                        connector,
                        parent_type.name().clone(),
                        field_def.ty.inner_named_type().clone(),
                    )?;
                }
                ConnectedElement::Type { type_ref } => {
                    SchemaVisitor::new(
                        self.original_schema,
                        &mut schema,
                        &self.directive_deny_list,
                    )
                    .walk((
                        ObjectTypeDefinitionPosition {
                            type_name: type_ref.name().clone(),
                        },
                        connector
                            .selection
                            .next_subselection()
                            .cloned()
                            .ok_or_else(|| {
                                FederationError::internal("empty selections are not allowed")
                            })?,
                    ))?;

                    // we need a Query root field to be valid
                    self.ensure_query_root_type(&mut schema, &query_alias, None)?;

                    // Process any outputs needed by the connector
                    self.process_outputs(
                        &mut schema,
                        connector,
                        type_ref.name().clone(),
                        type_ref.name().clone(),
                    )?;
                }
            }

            Ok(schema)
        }

        /// Process all input types
        ///
        /// Inputs can include leaf types as well as custom inputs.
        fn process_inputs(
            &self,
            to_schema: &mut FederationSchema,
            arguments: &[Node<InputValueDefinition>],
        ) -> Result<(), FederationError> {
            // All inputs to a connector's field need to be carried over in order to always generate
            // valid subgraphs
            for arg in arguments {
                let arg_type_name = arg.ty.inner_named_type();
                let arg_type = self.original_schema.get_type(arg_type_name.clone())?;
                let arg_extended_type = arg_type.get(self.original_schema.schema())?;

                // If the input type isn't built in, then we need to carry it over, making sure to only walk
                // if we have a complex input since leaf types can just be copied over.
                if !arg_extended_type.is_built_in() {
                    match arg_type {
                        TypeDefinitionPosition::InputObject(input) => {
                            SchemaVisitor::new(
                                self.original_schema,
                                to_schema,
                                &self.directive_deny_list,
                            )
                            .walk(input)?;
                        }
                        other => self.insert_custom_leaf(to_schema, &other)?,
                    };
                }
            }

            Ok(())
        }

        // Process outputs needed by a connector
        //
        // By the time this method is called, all dependent types should exist for a connector,
        // including its direct inputs. Since each connector could select only a subset of its output
        // type, this method carries over each output type as seen by the selection defined on the connector.
        fn process_outputs(
            &self,
            to_schema: &mut FederationSchema,
            connector: &Connector,
            parent_type_name: Name,
            output_type_name: Name,
        ) -> Result<(), FederationError> {
            let resolvable_key = connector
                .resolvable_key(self.original_schema.schema())
                .map_err(|_| FederationError::internal("error creating resolvable key"))?;

            let Some(resolvable_key) = resolvable_key else {
                return self.copy_interface_object_keys(output_type_name, to_schema);
            };

            let parent_type = self.original_schema.get_type(parent_type_name)?;
            let output_type = to_schema.get_type(output_type_name)?;
            let key_for_type = match &connector.entity_resolver {
                Some(EntityResolver::Explicit) => output_type,
                _ => parent_type,
            };

            let parsed = JSONSelection::parse_with_spec(
                &resolvable_key.serialize().no_indent().to_string(),
                connector.spec,
            )
            .map_err(|e| FederationError::internal(format!("error parsing key: {e}")))?;

            let visitor =
                SchemaVisitor::new(self.original_schema, to_schema, &self.directive_deny_list);

            let output_type = match &key_for_type {
                TypeDefinitionPosition::Object(object) => object,

                other => {
                    return Err(FederationError::internal(format!(
                        "connector output types currently only support object types: found {}",
                        other.type_name()
                    )));
                }
            };

            // This adds child types for all key fields
            visitor.walk((
                output_type.clone(),
                parsed
                    .next_subselection()
                    .cloned()
                    .ok_or_else(|| FederationError::internal("empty selections are not allowed"))?,
            ))?;

            // This actually adds the key fields if necessary, which is only
            // when depending on sibling fields.
            if let Some(sub) = parsed.next_subselection() {
                for named in sub.selections_iter() {
                    for field_name in named.names() {
                        let field_def = self
                            .original_schema
                            .schema()
                            .type_field(key_for_type.type_name(), field_name)
                            .map_err(|_| {
                                FederationError::internal(format!(
                                    "field {} not found on type {}",
                                    field_name,
                                    key_for_type.type_name()
                                ))
                            })?;

                        insert_field_if_missing(
                            &key_for_type,
                            Name::new(field_name)?,
                            Component::new(FieldDefinition {
                                description: field_def.description.clone(),
                                name: field_def.name.clone(),
                                arguments: field_def.arguments.clone(),
                                ty: field_def.ty.clone(),
                                directives: filter_directives(
                                    &self.directive_deny_list,
                                    &field_def.directives,
                                ),
                            }),
                            to_schema,
                        )?;
                    }
                }
            };

            // If we have marked keys as being necessary for this output type, add them as an `@key`
            // directive now.
            let key_directive = Directive {
                name: self.key_name.clone(),
                arguments: vec![Node::new(Argument {
                    name: name!("fields"),
                    value: Node::new(Value::String(
                        resolvable_key.serialize().no_indent().to_string(),
                    )),
                })],
            };

            match &key_for_type {
                TypeDefinitionPosition::Object(o) => {
                    o.insert_directive(to_schema, Component::new(key_directive))?;
                }
                TypeDefinitionPosition::Interface(i) => {
                    i.insert_directive(to_schema, Component::new(key_directive.clone()))?;
                    // Federation requires implementing types to also have the interface's @key
                    if let Some(implementers) = self
                        .original_schema
                        .schema()
                        .implementers_map()
                        .get(&i.type_name)
                    {
                        for implementer in &implementers.objects {
                            let obj_pos = ObjectTypeDefinitionPosition {
                                type_name: implementer.clone(),
                            };
                            obj_pos.insert_directive(
                                to_schema,
                                Component::new(key_directive.clone()),
                            )?;
                        }
                    }
                }
                _ => {
                    return Err(FederationError::internal(
                        "keys cannot be added to scalars, unions, enums, or input objects",
                    ));
                }
            }

            Ok(())
        }

        /// If the type has @interfaceObject and it doesn't have a key at this point
        /// we'll need to add a key — this is a requirement for using @interfaceObject.
        /// For now we'll just copy over keys from the original supergraph as resolvable: false
        /// but we need to think through the implications of that.
        fn copy_interface_object_keys(
            &self,
            type_name: Name,
            to_schema: &mut FederationSchema,
        ) -> Result<(), FederationError> {
            let Some(original_output_type) = self.original_schema.schema().get_object(&type_name)
            else {
                return Ok(());
            };

            let is_interface_object = original_output_type
                .directives
                .iter()
                .any(|d| d.name == self.interface_object_name);

            if is_interface_object {
                let pos = ObjectTypeDefinitionPosition {
                    type_name: original_output_type.name.clone(),
                };

                for key in original_output_type
                    .directives
                    .iter()
                    .filter(|d| d.name == self.key_name)
                {
                    let key_fields = key
                        .argument_by_name("fields", self.original_schema.schema())
                        .map_err(|_| internal_error!("@key(fields:) argument missing"))?;
                    let key = Directive {
                        name: key.name.clone(),
                        arguments: vec![
                            Node::new(Argument {
                                name: name!("fields"),
                                value: key_fields.clone(),
                            }),
                            Node::new(Argument {
                                name: name!("resolvable"),
                                value: Node::new(Value::Boolean(false)),
                            }),
                        ],
                    };
                    pos.insert_directive(to_schema, Component::new(key))?;
                }
            }

            Ok(())
        }

        /// Inserts a custom leaf type into the schema
        ///
        /// This errors if called with a non-leaf type.
        fn insert_custom_leaf(
            &self,
            to_schema: &mut FederationSchema,
            r#type: &TypeDefinitionPosition,
        ) -> Result<(), FederationError> {
            match r#type {
                TypeDefinitionPosition::Scalar(scalar) => {
                    let def = scalar.get(self.original_schema.schema())?;
                    let def = ScalarType {
                        description: def.description.clone(),
                        name: def.name.clone(),
                        directives: filter_directives(&self.directive_deny_list, &def.directives),
                    };

                    try_pre_insert!(to_schema, scalar)?;
                    try_insert!(to_schema, scalar, Node::new(def))
                }
                TypeDefinitionPosition::Enum(r#enum) => {
                    let def = r#enum.get(self.original_schema.schema())?;
                    let def = EnumType {
                        description: def.description.clone(),
                        name: def.name.clone(),
                        directives: filter_directives(&self.directive_deny_list, &def.directives),
                        values: def.values.clone(),
                    };

                    try_pre_insert!(to_schema, r#enum)?;
                    try_insert!(to_schema, r#enum, Node::new(def))
                }

                other => Err(FederationError::internal(format!(
                    "expected a leaf, found: {}",
                    other.type_name(),
                ))),
            }
        }

        /// Insert the parent type and field definition for a connector
        fn insert_object_and_field(
            &self,
            to_schema: &mut FederationSchema,
            field_parent: &ObjectTypeDefinitionPosition,
            field: impl AsRef<FieldDefinition>,
        ) -> Result<(), FederationError> {
            let original = field.as_ref();

            let parent_type = field_parent.get(self.original_schema.schema())?;

            try_pre_insert!(to_schema, field_parent)?;
            let field_def = FieldDefinition {
                description: original.description.clone(),
                name: original.name.clone(),
                arguments: original.arguments.clone(),
                ty: original.ty.clone(),
                directives: filter_directives(&self.directive_deny_list, &original.directives),
            };
            try_insert!(
                to_schema,
                field_parent,
                Node::new(ObjectType {
                    description: parent_type.description.clone(),
                    name: parent_type.name.clone(),
                    implements_interfaces: parent_type.implements_interfaces.clone(),
                    directives: filter_directives(
                        &self.directive_deny_list,
                        &parent_type.directives,
                    ),
                    fields: Default::default()
                })
            )?;

            let pos = ObjectFieldDefinitionPosition {
                type_name: parent_type.name.clone(),
                field_name: field_def.name.clone(),
            };

            pos.insert(to_schema, field_def.into())?;

            Ok(())
        }

        /// Insert a query root type for a connect field
        ///
        /// If the connector is not defined on a Query root field, we'll need to
        /// construct a dummy field to make a valid schema.
        ///
        /// ```graphql
        /// type Query {
        ///   _: ID @shareable @inaccessible
        /// }
        /// ```
        ///
        /// Note: This would probably be better off expanding the query to have
        /// an _entities vs. adding an inaccessible field.
        fn ensure_query_root_type(
            &self,
            to_schema: &mut FederationSchema,
            query_alias: &Name,
            parent_type_name: Option<&Name>,
        ) -> Result<(), FederationError> {
            if parent_type_name.is_none_or(|name| name != query_alias) {
                let query = ObjectTypeDefinitionPosition {
                    type_name: query_alias.clone(),
                };

                let dummy_field_def = FieldDefinition {
                    description: None,
                    name: name!("_"),
                    arguments: Vec::new(),
                    ty: ty!(ID),
                    directives: ast::DirectiveList(vec![Node::new(Directive {
                        name: name!("federation__inaccessible"),
                        arguments: Vec::new(),
                    })]),
                };

                query.pre_insert(to_schema)?;
                query.insert(
                    to_schema,
                    Node::new(ObjectType {
                        description: None,
                        name: query_alias.clone(),
                        implements_interfaces: IndexSet::with_hasher(Default::default()),
                        directives: DirectiveList::new(),
                        fields: IndexMap::from_iter([(
                            dummy_field_def.name.clone(),
                            Component::new(dummy_field_def),
                        )]),
                    }),
                )?;
            }

            SchemaRootDefinitionPosition {
                root_kind: SchemaRootDefinitionKind::Query,
            }
            .insert(
                to_schema,
                ComponentName {
                    origin: ComponentOrigin::Definition,
                    name: query_alias.clone(),
                },
            )?;

            Ok(())
        }

        /// Adds the mutation root type to the schema definition if necessary
        fn ensure_mutation_root_type(
            &self,
            to_schema: &mut FederationSchema,
            mutation_alias: &Name,
            parent_type_name: &Name,
        ) -> Result<(), FederationError> {
            if mutation_alias == parent_type_name
                && to_schema.get_type(mutation_alias.clone()).is_ok()
            {
                let mutation_root = SchemaRootDefinitionPosition {
                    root_kind: SchemaRootDefinitionKind::Mutation,
                };
                mutation_root.insert(
                    to_schema,
                    ComponentName {
                        origin: ComponentOrigin::Definition,
                        name: mutation_alias.clone(),
                    },
                )?;
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
