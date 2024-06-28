use std::sync::Arc;

use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::NodeStr;
use apollo_compiler::Schema;
use carryover::carryover_directives;
use indexmap::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;

use crate::error::FederationError;
use crate::link::Link;
use crate::merge::merge_subgraphs;
use crate::schema::FederationSchema;
use crate::sources::connect::ConnectSpecDefinition;
use crate::sources::connect::Connector;
use crate::subgraph::Subgraph;
use crate::subgraph::ValidSubgraph;
use crate::ApiSchemaOptions;
use crate::Supergraph;
use crate::ValidFederationSubgraph;

mod carryover;
mod visitor;
use visitor::ToSchemaVisitor;

pub struct Connectors {
    pub by_service_name: Arc<IndexMap<NodeStr, Connector>>,
    pub labels_by_service_name: Arc<IndexMap<String, String>>,
}

/// The result of a supergraph expansion of connect-aware subgraphs
pub enum ExpansionResult {
    /// The supergraph had some subgraphs that were expanded
    Expanded {
        raw_sdl: String,
        api_schema: Valid<Schema>,
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
        // TODO: Get these options from the router?
        include_defer: true,
        include_stream: false,
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
    let new_supergraph = merge_subgraphs(all_subgraphs).map_err(|e| {
        FederationError::internal(format!("could not merge expanded subgraphs: {e:?}"))
    })?;

    let mut new_supergraph = FederationSchema::new(new_supergraph.schema.into_inner())?;
    carryover_directives(&supergraph.schema, &mut new_supergraph)
        .map_err(|_e| FederationError::internal("could not carry over directives"))?;

    let connectors_by_service_name: IndexMap<NodeStr, Connector> = connect_subgraphs
        .into_iter()
        .map(|(connector, sub)| (NodeStr::new(&sub.name), connector))
        .collect();

    let labels_by_service_name = connectors_by_service_name
        .iter()
        .map(|(service_name, connector)| (service_name.to_string(), connector.id.label.clone()))
        .collect();

    Ok(ExpansionResult::Expanded {
        raw_sdl: new_supergraph.schema().serialize().to_string(),
        api_schema: api_schema.schema().clone(),
        connectors: Connectors {
            by_service_name: Arc::new(connectors_by_service_name),
            labels_by_service_name: Arc::new(labels_by_service_name),
        },
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

/// Filter out directives from a directive list
fn filter_directives<'a, D, I, U>(deny_list: &IndexSet<&Name>, directives: D) -> U
where
    D: IntoIterator<Item = &'a I>,
    I: 'a + AsRef<Directive> + Clone,
    U: FromIterator<I>,
{
    directives
        .into_iter()
        .filter(|d| !deny_list.contains(&d.as_ref().name))
        .cloned()
        .collect()
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

    use super::filter_directives;
    use super::ToSchemaVisitor;
    use crate::error::FederationError;
    use crate::link::Link;
    use crate::query_graph::extract_subgraphs_from_supergraph::new_empty_fed_2_subgraph_schema;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;
    use crate::schema::position::SchemaRootDefinitionPosition;
    use crate::schema::FederationSchema;
    use crate::schema::ObjectFieldDefinitionPosition;
    use crate::schema::ObjectOrInterfaceFieldDefinitionPosition;
    use crate::schema::ValidFederationSchema;
    use crate::sources::connect::ConnectSpecDefinition;
    use crate::sources::connect::Connector;
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

        /// Expand an object into the schema, copying over fields / types specified by the
        /// JSONSelection.
        fn expand_object(
            &self,
            to_schema: &mut FederationSchema,
            object_field: &ObjectFieldDefinitionPosition,
            connector: &Connector,
        ) -> Result<(), FederationError> {
            let field_def = object_field.get(self.original_schema.schema())?;

            // We first need to create the type pointed at by the current field
            let output_name = field_def.ty.inner_named_type().clone();
            let output_type = self.original_schema.get_type(output_name)?;
            let extended_output_type = output_type.get(self.original_schema.schema())?;

            // If the type is built-in, then there isn't anything that we need to do for the output type
            let directive_deny_list = IndexSet::from([&self.connect_name, &self.source_name]);
            if !extended_output_type.is_built_in() {
                let visitor = ToSchemaVisitor::new(
                    self.original_schema,
                    to_schema,
                    output_type,
                    extended_output_type,
                    &directive_deny_list,
                );

                connector.selection.visit(visitor)?;
            }

            let parent = object_field.parent();
            let parent_type = parent.get(self.original_schema.schema())?;

            let field_type = FieldDefinition {
                description: field_def.description.clone(),
                name: field_def.name.clone(),
                arguments: field_def.arguments.clone(),
                ty: field_def.ty.clone(),
                directives: filter_directives(&directive_deny_list, &field_def.directives),
            };
            let connector_parent_type = ObjectType {
                description: parent_type.description.clone(),
                name: parent_type.name.clone(),
                implements_interfaces: parent_type.implements_interfaces.clone(),
                directives: filter_directives(&directive_deny_list, &parent_type.directives),
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
        ///
        /// The inserted dummy query looks like the schema below:
        /// ```graphql
        /// type Query {
        ///   _: ID @shareable @inaccessible
        /// }
        /// ```
        ///
        /// Note: This would probably be better off expanding the query to have
        /// an __entries vs. adding an inaccessible field.
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
