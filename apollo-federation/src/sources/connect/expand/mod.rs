use apollo_compiler::validation::Valid;
use apollo_compiler::NodeStr;
use apollo_compiler::Schema;
use indexmap::IndexMap;
use itertools::Itertools;

use crate::error::FederationError;
use crate::link::Link;
use crate::merge::merge_subgraphs;
use crate::sources::connect::ConnectSpecDefinition;
use crate::sources::connect::Connector;
use crate::subgraph::Subgraph;
use crate::subgraph::ValidSubgraph;
use crate::ApiSchemaOptions;
use crate::Supergraph;
use crate::ValidFederationSubgraph;

/// The result of a supergraph expansion of connect-aware subgraphs
pub enum ExpansionResult {
    /// The supergraph had some subgraphs that were expanded
    Expanded {
        raw_sdl: String,
        api_schema: Valid<Schema>,
        connectors_by_service_name: IndexMap<NodeStr, Connector>,
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
/// off of existing functionality in a reproducable way.
pub fn expand_connectors(supergraph_str: &str) -> Result<ExpansionResult, FederationError> {
    // TODO: Don't rely on finding the URL manually to short out
    let connect_url = ConnectSpecDefinition::identity();
    let connect_url = format!("{}/{}/v", connect_url.domain, connect_url.name);
    if !supergraph_str.contains(&connect_url) {
        return Ok(ExpansionResult::Unchanged);
    }

    let supergraph = Supergraph::new(supergraph_str)?;
    let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
        include_defer: true,
        include_stream: true,
    })?;

    let (connect_subgraphs, graphql_subgraphs): (Vec<_>, Vec<_>) = supergraph
        .extract_subgraphs()?
        .into_iter()
        .partition_map(|(_, sub)| {
            match ConnectSpecDefinition::get_from_schema(sub.schema.schema()) {
                Ok(Some((_, link))) if contains_connectors(&link, &sub) => {
                    either::Either::Left((link, sub))
                }
                _ => either::Either::Right(ValidSubgraph::from(sub)),
            }
        });

    // Expand just the connector subgraphs
    let connect_subgraphs = connect_subgraphs
        .into_iter()
        .map(|(link, sub)| split_subgraph(&link, sub))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect_vec();

    // Merge the subgraphs into one supergraph
    let all_subgraphs = graphql_subgraphs
        .iter()
        .chain(connect_subgraphs.iter().map(|(_, sub)| sub))
        .collect();
    let supergraph = merge_subgraphs(all_subgraphs).map_err(|e| {
        FederationError::internal(format!("could not merge expanded subgraphs: {e:?}"))
    })?;

    let connectors_by_service_name: IndexMap<NodeStr, Connector> = connect_subgraphs
        .into_iter()
        .map(|(connector, sub)| (NodeStr::new(&sub.name), connector))
        .collect();

    Ok(ExpansionResult::Expanded {
        raw_sdl: supergraph.schema.serialize().to_string(),
        api_schema: api_schema.schema().clone(),
        connectors_by_service_name,
    })
}

fn contains_connectors(link: &Link, subgraph: &ValidFederationSubgraph) -> bool {
    let connect_name = ConnectSpecDefinition::connect_directive_name(link);
    let source_name = ConnectSpecDefinition::source_directive_name(link);

    subgraph
        .schema
        .get_directive_definitions()
        .any(|directive| {
            directive.directive_name == connect_name || directive.directive_name == source_name
        })
}

/// Split up a subgraph so that each connector directive becomes its own subgraph.
///
/// Subgraphs passed to this function should contain connector directives.
fn split_subgraph(
    link: &Link,
    subgraph: ValidFederationSubgraph,
) -> Result<Vec<(Connector, ValidSubgraph)>, FederationError> {
    let connector_map =
        Connector::from_valid_schema(&subgraph.schema, NodeStr::new(&subgraph.name))?;

    let expander = helpers::Expander::new(link, &subgraph);
    connector_map
        .into_iter()
        .map(|(id, connector)| {
            // Build a subgraph using only the necessary fields from the directive
            let schema = expander.expand(&connector)?;
            let subgraph = Subgraph::new(
                id.synthetic_name().as_str(),
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

mod helpers {
    use apollo_compiler::ast;
    use apollo_compiler::ast::FieldDefinition;
    use apollo_compiler::ast::Name;
    use apollo_compiler::name;
    use apollo_compiler::schema::Component;
    use apollo_compiler::schema::ComponentName;
    use apollo_compiler::schema::ComponentOrigin;
    use apollo_compiler::schema::DirectiveList;
    use apollo_compiler::schema::ObjectType;
    use apollo_compiler::Node;
    use apollo_compiler::NodeStr;
    use indexmap::IndexMap;
    use indexmap::IndexSet;

    use crate::error::FederationError;
    use crate::link::Link;
    use crate::query_graph::extract_subgraphs_from_supergraph::new_empty_fed_2_subgraph_schema;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;
    use crate::schema::position::SchemaRootDefinitionPosition;
    use crate::schema::position::TypeDefinitionPosition;
    use crate::schema::FederationSchema;
    use crate::schema::ObjectFieldDefinitionPosition;
    use crate::schema::ObjectOrInterfaceFieldDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use crate::sources::connect::ConnectSpecDefinition;
    use crate::sources::connect::Connector;
    use crate::sources::connect::JSONSelection;
    use crate::ValidFederationSubgraph;

    /// A helper struct for expanding a subgraph into one per connect directive.
    pub(super) struct Expander<'a> {
        /// The name of the connect directive, possibly aliased.
        connect_name: Name,

        /// The name of the connect directive, possibly aliased.
        source_name: Name,

        /// The original schema that contains connect directives
        original_schema: &'a ValidFederationSchema,
    }

    impl<'a> Expander<'a> {
        pub(super) fn new(link: &Link, subgraph: &'a ValidFederationSubgraph) -> Expander<'a> {
            let connect_name = ConnectSpecDefinition::connect_directive_name(link);
            let source_name = ConnectSpecDefinition::source_directive_name(link);

            Self {
                connect_name,
                source_name,
                original_schema: &subgraph.schema,
            }
        }

        /// Build an expanded subgraph for the supplied connector
        pub(super) fn expand(
            &self,
            connector: &Connector,
        ) -> Result<FederationSchema, FederationError> {
            let mut schema = new_empty_fed_2_subgraph_schema()?;

            // Add the parent type for this connector
            let field = &connector.id.directive.field;
            match field {
                ObjectOrInterfaceFieldDefinitionPosition::Object(o) => {
                    self.expand_object(&mut schema, o, connector)?
                }
                ObjectOrInterfaceFieldDefinitionPosition::Interface(i) => {
                    todo!("not yet done for interface {i}")
                }
            };

            // Add a query root with dummy information if the connect directive is not on a query
            // field.
            let query_alias = self
                .original_schema
                .schema()
                .schema_definition
                .query
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or(name!("Query"));
            if field.parent().type_name() != &query_alias {
                self.insert_dummy_query(&mut schema, query_alias)?;
            }

            // Add the query root
            let root = SchemaRootDefinitionPosition {
                root_kind: SchemaRootDefinitionKind::Query,
            };
            root.insert(
                &mut schema,
                ComponentName {
                    origin: ComponentOrigin::Definition,
                    name: name!("Query"),
                },
            )?;

            Ok(schema)
        }

        fn expand_object(
            &self,
            to_schema: &mut FederationSchema,
            object: &ObjectFieldDefinitionPosition,
            connector: &Connector,
        ) -> Result<(), FederationError> {
            let field_type = object.get(self.original_schema.schema())?;

            // We first need to create the type pointed at by the current field
            let output_name = field_type.ty.inner_named_type().clone();
            let output_type = self.original_schema.get_type(output_name)?;
            let extended_output_type = output_type.get(self.original_schema.schema())?;

            // If the type is built-in, then there isn't anything that we need to do for the output type
            if !extended_output_type.is_built_in() {
                match output_type {
                    TypeDefinitionPosition::Object(out) => {
                        // Extract the needed fields from the selection
                        let JSONSelection::Named(ref selections) = connector.selection else {
                            unimplemented!()
                        };
                        let mut field_selections = IndexMap::new();
                        for selection in selections.selections_iter() {
                            let as_name = Name::new(selection.name())?;
                            let field = out
                                .field(as_name.clone())
                                .get(self.original_schema.schema())?;
                            let field = FieldDefinition {
                                description: field.description.clone(),
                                name: field.name.clone(),
                                arguments: field.arguments.clone(),
                                ty: field.ty.clone(),
                                directives: ast::DirectiveList::new(),
                            };

                            field_selections.insert(as_name, Component::new(field));
                        }

                        let out_def = out.get(self.original_schema.schema())?;
                        let out_type = ObjectType {
                            description: out_def.description.clone(),
                            name: out_def.name.clone(),
                            implements_interfaces: out_def.implements_interfaces.clone(),
                            directives: DirectiveList::new(),
                            fields: field_selections,
                        };

                        out.pre_insert(to_schema)?;
                        out.insert(to_schema, Node::new(out_type))?
                    }
                    o => todo!("not yet done for {o:?}"),
                };
            }

            let parent = object.parent();
            let parent_type = parent.get(self.original_schema.schema())?;
            // let parent_directives = parent_type
            //     .directives
            //     .into_iter()
            //     .filter(|d| d.name != source_name && d.name != connect_name)
            //     .collect();

            let field_type = FieldDefinition {
                description: field_type.description.clone(),
                name: field_type.name.clone(),
                arguments: field_type.arguments.clone(),
                ty: field_type.ty.clone(),
                directives: ast::DirectiveList::new(),
            };
            let connector_parent_type = ObjectType {
                description: parent_type.description.clone(),
                name: parent_type.name.clone(),
                implements_interfaces: parent_type.implements_interfaces.clone(),
                directives: DirectiveList::new(),
                fields: IndexMap::from([(
                    field_type.ty.inner_named_type().clone(),
                    Component::new(field_type),
                )]),
            };

            parent.pre_insert(to_schema)?;
            parent.insert(to_schema, Node::new(connector_parent_type))?;

            Ok(())
        }

        /// Inserts a dummy root-level query to the schema to pass validation.
        fn insert_dummy_query(
            &self,
            to_schema: &mut FederationSchema,
            name: Name,
        ) -> Result<(), FederationError> {
            let field_name = name!("_");

            let dummy_query = ObjectTypeDefinitionPosition {
                type_name: name.clone(),
            };
            let dummy_field = Component::new(FieldDefinition {
                description: None,
                name: field_name.clone(),
                arguments: Vec::new(),
                ty: ast::Type::Named(ast::NamedType::new_unchecked(NodeStr::from_static(&"ID"))),
                directives: ast::DirectiveList(
                    [
                        name!("federation__shareable"),
                        name!("federation__inaccessible"),
                    ]
                    .into_iter()
                    .map(|name| {
                        Node::new(ast::Directive {
                            name,
                            arguments: Vec::new(),
                        })
                    })
                    .collect(),
                ),
            });

            dummy_query.pre_insert(to_schema)?;
            dummy_query.insert(
                to_schema,
                Node::new(ObjectType {
                    description: None,
                    name,
                    implements_interfaces: IndexSet::new(),
                    directives: DirectiveList::new(),
                    fields: IndexMap::from([(field_name, dummy_field)]),
                }),
            )?;

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
