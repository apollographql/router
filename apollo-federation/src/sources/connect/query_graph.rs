use apollo_compiler::ast::Name;
use indexmap::map::Entry;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::prelude::NodeIndex;

use super::models::Connector;
use super::selection_parser::Property;
use super::selection_parser::SubSelection;
use super::ConnectFederatedConcreteFieldQueryGraphEdge;
use super::ConnectFederatedConcreteQueryGraphNode;
use super::ConnectFederatedEnumQueryGraphNode;
use super::ConnectFederatedQueryGraphBuilder;
use super::ConnectFederatedSourceEnteringQueryGraphEdge;
use super::Selection;
use crate::error::FederationError;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::sources::connect::ConnectFederatedScalarQueryGraphNode;
use crate::sources::SourceFederatedConcreteFieldQueryGraphEdge;
use crate::sources::SourceFederatedConcreteQueryGraphNode;
use crate::sources::SourceFederatedEnumQueryGraphNode;
use crate::sources::SourceFederatedQueryGraphBuilderApi;
use crate::sources::SourceFederatedScalarQueryGraphNode;
use crate::sources::SourceFederatedSourceEnteringQueryGraphEdge;
use crate::sources::SourceId;
use crate::ValidFederationSubgraph;

impl SourceFederatedQueryGraphBuilderApi for ConnectFederatedQueryGraphBuilder {
    fn process_subgraph_schema(
        &self,
        subgraph: ValidFederationSubgraph,
        builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError> {
        // Extract the connects from the schema definition and map them to their `Connect` equivalent
        let connectors = Connector::from_valid_schema(&subgraph.schema, subgraph.name.into())?;

        for (id, connect) in connectors {
            // Inform the builder that every node / edge from here out are part of the current connect directive
            builder.add_and_set_current_source(SourceId::Connect(id.clone()))?;

            let ObjectOrInterfaceFieldDefinitionPosition::Object(field_def_pos) =
                id.directive.field
            else {
                return Err(FederationError::internal(
                    "connect directives must be on objects",
                ));
            };

            // Make a node for the entrypoint of this field
            let parent_node = builder.add_concrete_node(
                field_def_pos.type_name.clone(),
                SourceFederatedConcreteQueryGraphNode::Connect(
                    ConnectFederatedConcreteQueryGraphNode::ConnectParent {
                        subgraph_type: field_def_pos.parent().clone(),
                    },
                ),
            )?;

            // Mark this entrypoint as being externally accessible to other resolvers
            builder.add_source_entering_edge(
                parent_node,
                None,
                SourceFederatedSourceEnteringQueryGraphEdge::Connect(
                    ConnectFederatedSourceEnteringQueryGraphEdge::ConnectParent {
                        subgraph_type: field_def_pos.parent().clone(),
                    },
                ),
            )?;

            // Process the field, constructing the rest of the graph for its selections
            let field_output_type_name = field_def_pos
                .get(subgraph.schema.schema())?
                .ty
                .inner_named_type();
            let field_output_type_pos = subgraph.schema.get_type(field_output_type_name.clone())?;
            let field_node = process_selection(
                connect.selection,
                field_output_type_pos,
                &subgraph.schema,
                builder,
            )?;

            // Make an edge from the parent into our new subgraph
            builder.add_concrete_field_edge(
                parent_node,
                field_node,
                field_def_pos.field_name.clone(),
                IndexSet::new(),
                SourceFederatedConcreteFieldQueryGraphEdge::Connect(
                    ConnectFederatedConcreteFieldQueryGraphEdge::Connect {
                        subgraph_field: field_def_pos,
                    },
                ),
            )?;
        }

        Ok(())
    }
}

/// Processes a connect selection
///
/// This method creates nodes from selection parameters of a field decorated by
/// a connect directive, making sure to reuse nodes if possible.
fn process_selection(
    selection: Selection,
    field_output_type_pos: TypeDefinitionPosition,
    subgraph_schema: &ValidFederationSchema,
    builder: &mut impl IntraSourceQueryGraphBuilderApi,
) -> Result<NodeIndex<u32>, FederationError> {
    // Keep a cache to reuse nodes
    let mut node_cache: IndexMap<Name, NodeIndex<u32>> = IndexMap::new();

    // Get the type of the field
    let field_ty = field_output_type_pos.get(subgraph_schema.schema())?;

    // Custom scalars are easy, so handle them first
    if field_ty.is_scalar() && !field_ty.is_built_in() {
        // Note: the if condition checked that this is a scalar, so trying to unwrap to anything else
        // is impossible.
        let TypeDefinitionPosition::Scalar(scalar_field_ty) = field_output_type_pos else {
            return Err(FederationError::internal("scalar wasn't really a scalar"));
        };

        return builder.add_scalar_node(
            field_ty.name().clone(),
            SourceFederatedScalarQueryGraphNode::Connect(
                ConnectFederatedScalarQueryGraphNode::CustomScalarSelectionRoot {
                    subgraph_type: scalar_field_ty,
                    selection,
                },
            ),
        );
    }

    // If we aren't a custom scalar, then look at the selection to see what to attempt
    match selection {
        Selection::Path(path) => match field_output_type_pos {
            TypeDefinitionPosition::Enum(enum_type) => {
                // Create the node for this enum
                builder.add_enum_node(
                    field_ty.name().clone(),
                    SourceFederatedEnumQueryGraphNode::Connect(
                        ConnectFederatedEnumQueryGraphNode::SelectionRoot {
                            subgraph_type: enum_type,
                            property_path: path.collect_paths(),
                        },
                    ),
                )
            }
            TypeDefinitionPosition::Scalar(scalar_type) => {
                // Create the node for this enum
                builder.add_scalar_node(
                    field_ty.name().clone(),
                    SourceFederatedScalarQueryGraphNode::Connect(
                        ConnectFederatedScalarQueryGraphNode::SelectionRoot {
                            subgraph_type: scalar_type,
                            property_path: path.collect_paths(),
                        },
                    ),
                )
            }

            _ => {
                // If we don't have either of the above, then we must have a subselection
                let Some(sub) = path.next_subselection() else {
                    return Err(FederationError::internal(
                        "expected subselection for leaf type",
                    ));
                };

                process_subselection(
                    sub,
                    field_output_type_pos,
                    subgraph_schema,
                    builder,
                    &mut node_cache,
                    Some(path.collect_paths()),
                )
            }
        },
        Selection::Named(sub) => {
            // Make sure that we aren't selecting sub fields from simple types
            if field_ty.is_scalar() || field_ty.is_enum() {
                return Err(FederationError::internal(
                    "leaf types cannot have subselections",
                ));
            }

            // Grab what we need and return the root node
            process_subselection(
                &sub,
                field_output_type_pos,
                subgraph_schema,
                builder,
                &mut node_cache,
                Some(Vec::new()),
            )
        }
    }
}

fn process_subselection(
    sub: &SubSelection,
    field_output_type_pos: TypeDefinitionPosition,
    subgraph_schema: &ValidFederationSchema,
    builder: &mut impl IntraSourceQueryGraphBuilderApi,
    node_cache: &mut IndexMap<Name, NodeIndex<u32>>,
    properties_path: Option<Vec<Property>>,
) -> Result<NodeIndex<u32>, FederationError> {
    // Get the type of the field
    let field_ty = field_output_type_pos.get(subgraph_schema.schema())?;

    // For milestone 1 we don't need to support anything other than objects...
    let TypeDefinitionPosition::Object(object_pos) = field_output_type_pos else {
        return Err(FederationError::internal(
            "expected subselection to be of a GraphQL object",
        ));
    };
    let object_type = object_pos.get(subgraph_schema.schema())?;
    let field_type_pos = object_pos.field(field_ty.name().clone());

    // Create the root node for this object
    let object_node = builder.add_concrete_node(
        field_ty.name().clone(),
        SourceFederatedConcreteQueryGraphNode::Connect(
            properties_path
                .map(
                    |props| ConnectFederatedConcreteQueryGraphNode::SelectionRoot {
                        subgraph_type: object_pos.clone(),
                        property_path: props,
                    },
                )
                .unwrap_or(ConnectFederatedConcreteQueryGraphNode::SelectionChild {
                    subgraph_type: object_pos.clone(),
                }),
        ),
    )?;

    // Handle all named selections
    for selection in sub.selections.iter() {
        // Make sure that we have a field on the object type that matches the alias (or the name itself)
        let alias = selection.name();
        let Some(selection_field) = object_type.fields.get(alias) else {
            return Err(FederationError::internal(format!(
                "expected field `{alias}` to exist on GraphQL type `{}`",
                object_type.name
            )));
        };
        let selection_type =
            subgraph_schema.get_type(selection_field.ty.inner_named_type().clone())?;
        let subgraph_field_pos = field_type_pos.clone();

        // Extract the property chain for this selection
        let properties = selection.property_path();
        let next_subselection = selection.next_subselection();

        // Now add sub type info to the graph
        match selection_type {
            TypeDefinitionPosition::Enum(ref r#enum) => {
                // An enum cannot have sub selections, so enforce that now
                if next_subselection.is_some() {
                    return Err(FederationError::internal(
                        "an enum cannot have a subselection",
                    ));
                }

                // Create the scalar node (or grab it from the cache)
                let enum_node = match node_cache.entry(r#enum.type_name.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let node = builder.add_enum_node(
                            r#enum.type_name.clone(),
                            SourceFederatedEnumQueryGraphNode::Connect(
                                ConnectFederatedEnumQueryGraphNode::SelectionChild {
                                    subgraph_type: r#enum.clone(),
                                },
                            ),
                        )?;

                        e.insert(node)
                    }
                };

                // Link the field to the object node
                builder.add_concrete_field_edge(
                    object_node,
                    *enum_node,
                    selection_field.name.clone(),
                    IndexSet::new(),
                    SourceFederatedConcreteFieldQueryGraphEdge::Connect(
                        ConnectFederatedConcreteFieldQueryGraphEdge::Selection {
                            subgraph_field: subgraph_field_pos,
                            property_path: properties,
                        },
                    ),
                )?;
            }
            TypeDefinitionPosition::Scalar(ref scalar) => {
                // Custom scalars need to be handled differently
                if !scalar.get(subgraph_schema.schema())?.is_built_in() {
                    return Err(FederationError::internal(
                        "custom scalars are not yet handled",
                    ));
                }

                // A scalar cannot have sub selections, so enforce that now
                if next_subselection.is_some() {
                    return Err(FederationError::internal(
                        "a scalar cannot have a subselection",
                    ));
                }

                // Create the scalar node (or grab it from the cache)
                let scalar_node = match node_cache.entry(scalar.type_name.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let node = builder.add_scalar_node(
                            scalar.type_name.clone(),
                            SourceFederatedScalarQueryGraphNode::Connect(
                                ConnectFederatedScalarQueryGraphNode::SelectionChild {
                                    subgraph_type: scalar.clone(),
                                },
                            ),
                        )?;

                        e.insert(node)
                    }
                };

                // Link the field to the object node
                builder.add_concrete_field_edge(
                    object_node,
                    *scalar_node,
                    selection_field.name.clone(),
                    IndexSet::new(),
                    SourceFederatedConcreteFieldQueryGraphEdge::Connect(
                        ConnectFederatedConcreteFieldQueryGraphEdge::Selection {
                            subgraph_field: subgraph_field_pos,
                            property_path: properties,
                        },
                    ),
                )?;
            }

            // The other types must be composite
            other => {
                // Since the type must be composite, there HAS to be a subselection
                let Some(subselection) = next_subselection else {
                    return Err(FederationError::internal(
                        "a composite type must have a subselection",
                    ));
                };

                let subselection_node = process_subselection(
                    subselection,
                    other,
                    subgraph_schema,
                    builder,
                    node_cache,
                    None,
                )?;

                // Link the field to the object node
                builder.add_concrete_field_edge(
                    object_node,
                    subselection_node,
                    selection_field.name.clone(),
                    IndexSet::new(),
                    SourceFederatedConcreteFieldQueryGraphEdge::Connect(
                        ConnectFederatedConcreteFieldQueryGraphEdge::Selection {
                            subgraph_field: subgraph_field_pos,
                            property_path: properties,
                        },
                    ),
                )?;
            }
        }
    }

    // Handle the optional star selection
    if let Some(_star) = sub.star.as_ref() {
        return Err(FederationError::internal(
            "star selection is not yet supported",
        ));
    }

    Ok(object_node)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use super::ConnectFederatedQueryGraphBuilder;
    use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
    use crate::schema::FederationSchema;
    use crate::sources::SourceFederatedQueryGraphBuilderApi;
    use crate::ValidFederationSubgraphs;

    fn get_subgraphs(supergraph_sdl: &str) -> ValidFederationSubgraphs {
        let schema = Schema::parse(supergraph_sdl, "supergraph.graphql").unwrap();
        let supergraph_schema = FederationSchema::new(schema).unwrap();
        extract_subgraphs_from_supergraph(&supergraph_schema, Some(true)).unwrap()
    }

    #[test]
    fn it_handles_a_simple_schema() {
        let federated_builder = ConnectFederatedQueryGraphBuilder;
        let mut mock_builder = mock::MockSourceQueryGraphBuilder::new();
        let subgraphs = get_subgraphs(include_str!("./tests/schemas/simple.graphql"));
        let (_, subgraph) = subgraphs.into_iter().next().unwrap();

        // Make sure that the tail data is correct
        federated_builder
            .process_subgraph_schema(subgraph, &mut mock_builder)
            .unwrap();

        // Make sure that our graph makes sense
        let as_dot = mock_builder.into_dot();
        assert_snapshot!(as_dot, @r###"
        digraph {
          subgraph cluster_0 {
            node [style = filled,color = white]

            0.0 [ label = ": _" ]

            style = filled
            color = lightgrey
            label = "Source-Aware Entrypoint"
          }
          subgraph cluster_1 {
            node [style = filled,color = white]

            1.0 [ label = "Node: Query" ]
            1.1 [ label = "Node: User" ]
            1.2 [ label = "Scalar: ID" ]
            1.3 [ label = "Scalar: String" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /users"
          }
          subgraph cluster_2 {
            node [style = filled,color = white]

            2.0 [ label = "Node: Query" ]
            2.1 [ label = "Node: Post" ]
            2.2 [ label = "Scalar: ID" ]
            2.3 [ label = "Scalar: String" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /posts"
          }
          0.0 -> 1.0 [ label = "Query" ]
          1.1 -> 1.2 [ label = "id" ]
          1.1 -> 1.3 [ label = "name" ]
          1.0 -> 1.1 [ label = "users" ]
          0.0 -> 2.0 [ label = "Query" ]
          2.1 -> 2.2 [ label = "id" ]
          2.1 -> 2.3 [ label = "title" ]
          2.1 -> 2.3 [ label = "body" ]
          2.0 -> 2.1 [ label = "posts" ]
        }
        "###);
    }

    #[test]
    fn it_handles_an_aliased_schema() {
        let federated_builder = ConnectFederatedQueryGraphBuilder;
        let mut mock_builder = mock::MockSourceQueryGraphBuilder::new();
        let subgraphs = get_subgraphs(include_str!("./tests/schemas/aliasing.graphql"));
        let (_, subgraph) = subgraphs.into_iter().next().unwrap();

        // Make sure that the tail data is correct
        federated_builder
            .process_subgraph_schema(subgraph, &mut mock_builder)
            .unwrap();

        // Make sure that our graph makes sense
        let as_dot = mock_builder.into_dot();
        assert_snapshot!(as_dot, @r###"
        digraph {
          subgraph cluster_0 {
            node [style = filled,color = white]

            0.0 [ label = ": _" ]

            style = filled
            color = lightgrey
            label = "Source-Aware Entrypoint"
          }
          subgraph cluster_1 {
            node [style = filled,color = white]

            1.0 [ label = "Node: Query" ]
            1.1 [ label = "Node: User" ]
            1.2 [ label = "Scalar: ID" ]
            1.3 [ label = "Scalar: String" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /users"
          }
          subgraph cluster_2 {
            node [style = filled,color = white]

            2.0 [ label = "Node: Query" ]
            2.1 [ label = "Node: Post" ]
            2.2 [ label = "Scalar: ID" ]
            2.3 [ label = "Scalar: String" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /posts"
          }
          0.0 -> 1.0 [ label = "Query" ]
          1.1 -> 1.2 [ label = "id" ]
          1.1 -> 1.3 [ label = "name: .username" ]
          1.0 -> 1.1 [ label = "users" ]
          0.0 -> 2.0 [ label = "Query" ]
          2.1 -> 2.2 [ label = "id" ]
          2.1 -> 2.3 [ label = "title: .\"body title\"" ]
          2.1 -> 2.3 [ label = "body: .summary" ]
          2.0 -> 2.1 [ label = "posts" ]
        }
        "###
        );
    }

    #[test]
    fn it_handles_a_cyclical_schema() {
        let federated_builder = ConnectFederatedQueryGraphBuilder;
        let mut mock_builder = mock::MockSourceQueryGraphBuilder::new();
        let subgraphs = get_subgraphs(include_str!("./tests/schemas/cyclical.graphql"));
        let (_, subgraph) = subgraphs.into_iter().next().unwrap();

        // Make sure that the tail data is correct
        federated_builder
            .process_subgraph_schema(subgraph, &mut mock_builder)
            .unwrap();

        // Make sure that our graph makes sense
        let as_dot = mock_builder.into_dot();
        assert_snapshot!(as_dot, @r###"
        digraph {
          subgraph cluster_0 {
            node [style = filled,color = white]

            0.0 [ label = ": _" ]

            style = filled
            color = lightgrey
            label = "Source-Aware Entrypoint"
          }
          subgraph cluster_1 {
            node [style = filled,color = white]

            1.0 [ label = "Node: Query" ]
            1.1 [ label = "Node: User" ]
            1.2 [ label = "Scalar: ID" ]
            1.3 [ label = "Scalar: String" ]
            1.4 [ label = "Node: User" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /users/1"
          }
          subgraph cluster_2 {
            node [style = filled,color = white]

            2.0 [ label = "Node: Query" ]
            2.1 [ label = "Node: User" ]
            2.2 [ label = "Scalar: ID" ]
            2.3 [ label = "Scalar: String" ]
            2.4 [ label = "Node: User" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /users/1"
          }
          0.0 -> 1.0 [ label = "Query" ]
          1.1 -> 1.2 [ label = "id" ]
          1.1 -> 1.3 [ label = "name" ]
          1.4 -> 1.2 [ label = "id" ]
          1.1 -> 1.4 [ label = "friends" ]
          1.0 -> 1.1 [ label = "me" ]
          0.0 -> 2.0 [ label = "Query" ]
          2.1 -> 2.2 [ label = "id" ]
          2.1 -> 2.3 [ label = "name" ]
          2.4 -> 2.2 [ label = "id" ]
          2.1 -> 2.4 [ label = "friends" ]
          2.0 -> 2.1 [ label = "user" ]
        }
        "###
        );
    }

    #[test]
    fn it_handles_a_nested_schema() {
        let federated_builder = ConnectFederatedQueryGraphBuilder;
        let mut mock_builder = mock::MockSourceQueryGraphBuilder::new();
        let subgraphs = get_subgraphs(include_str!("./tests/schemas/nested.graphql"));
        let (_, subgraph) = subgraphs.into_iter().next().unwrap();

        // Make sure that the tail data is correct
        federated_builder
            .process_subgraph_schema(subgraph, &mut mock_builder)
            .unwrap();

        // Make sure that our graph makes sense
        let as_dot = mock_builder.into_dot();
        assert_snapshot!(as_dot, @r###"
        digraph {
          subgraph cluster_0 {
            node [style = filled,color = white]

            0.0 [ label = ": _" ]

            style = filled
            color = lightgrey
            label = "Source-Aware Entrypoint"
          }
          subgraph cluster_1 {
            node [style = filled,color = white]

            1.0 [ label = "Node: Query" ]
            1.1 [ label = "Node: User" ]
            1.2 [ label = "Scalar: ID" ]
            1.3 [ label = "Node: UserInfo" ]
            1.4 [ label = "Scalar: String" ]
            1.5 [ label = "Node: UserAddress" ]
            1.6 [ label = "Scalar: Int" ]
            1.7 [ label = "Node: UserAvatar" ]

            style = filled
            color = lightgrey
            label = "connectors.json http: Get /users"
          }
          0.0 -> 1.0 [ label = "Query" ]
          1.1 -> 1.2 [ label = "id" ]
          1.3 -> 1.4 [ label = "name: .\"user full name\"" ]
          1.5 -> 1.4 [ label = "street: .street_line" ]
          1.5 -> 1.4 [ label = "state" ]
          1.5 -> 1.6 [ label = "zip" ]
          1.3 -> 1.5 [ label = "address: .addresses.main.address" ]
          1.7 -> 1.4 [ label = "large" ]
          1.7 -> 1.4 [ label = "thumbnail" ]
          1.3 -> 1.7 [ label = "avatar" ]
          1.1 -> 1.3 [ label = "info: .user_info" ]
          1.0 -> 1.1 [ label = "user" ]
        }
        "###
        );
    }

    mod mock {
        use std::fmt::Display;

        use apollo_compiler::ast::Name;
        use apollo_compiler::ast::NamedType;
        use indexmap::IndexSet;
        use itertools::Itertools;
        use petgraph::dot::Config;
        use petgraph::dot::Dot;
        use petgraph::prelude::EdgeIndex;
        use petgraph::prelude::NodeIndex;
        use petgraph::Graph;

        use crate::error::FederationError;
        use crate::schema::position::ObjectFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
        use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
        use crate::source_aware::federated_query_graph::SelfConditionIndex;
        use crate::sources::connect::selection_parser::Property;
        use crate::sources::connect::ConnectFederatedConcreteFieldQueryGraphEdge;
        use crate::sources::connect::ConnectFederatedSourceEnteringQueryGraphEdge;
        use crate::sources::connect::ConnectId;
        use crate::sources::SourceFederatedAbstractFieldQueryGraphEdge;
        use crate::sources::SourceFederatedConcreteFieldQueryGraphEdge;
        use crate::sources::SourceFederatedConcreteQueryGraphNode;
        use crate::sources::SourceFederatedEnumQueryGraphNode;
        use crate::sources::SourceFederatedQueryGraph;
        use crate::sources::SourceFederatedScalarQueryGraphNode;
        use crate::sources::SourceFederatedSourceEnteringQueryGraphEdge;
        use crate::sources::SourceFederatedTypeConditionQueryGraphEdge;
        use crate::sources::SourceId;

        /// A mock query Node
        #[derive(Clone)]
        struct MockNode {
            prefix: String,
            type_name: NamedType,
            source_id: SourceId,
        }
        impl Display for MockNode {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}: {}", self.prefix, self.type_name)
            }
        }

        /// A mock query edge
        struct MockEdge {
            field_name: Name,
            path: Vec<Property>,
        }
        impl Display for MockEdge {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.field_name)?;

                // Short out if we have no path to display
                if self.path.is_empty() {
                    return Ok(());
                }

                // Helper for checking name equality of a property
                fn is_name_eq(prop: &Property, other: &str) -> bool {
                    match prop {
                        Property::Field(f) => f == other,
                        Property::Quoted(q) => q == other,

                        // No string name will be equal to a number
                        Property::Index(_) => false,
                    }
                }

                // Short out if we have the same path as the identifier
                if self.path.len() == 1 && is_name_eq(&self.path[0], self.field_name.as_str()) {
                    return Ok(());
                }

                // Show the equivalent JSON path, if different from the field itself
                write!(f, ": ")?;
                for path in &self.path {
                    write!(f, "{path}")?;
                }

                Ok(())
            }
        }

        /// Mock implementation of [IntraSourceQueryGraphBuilder]
        pub struct MockSourceQueryGraphBuilder {
            graph: Graph<MockNode, MockEdge>,
            current_source: Option<SourceId>,
            entering_node: NodeIndex<u32>,
        }
        impl MockSourceQueryGraphBuilder {
            pub fn new() -> Self {
                let empty_name = Name::new("_").unwrap();

                let mut graph = Graph::new();
                let entering_node = graph.add_node(MockNode {
                    prefix: "".to_string(),
                    type_name: empty_name.clone(),
                    source_id: SourceId::Connect(ConnectId {
                        label: "Source-Aware Entrypoint".to_string(),
                        subgraph_name: "".to_string().into(),
                        directive: ObjectOrInterfaceFieldDirectivePosition {
                            field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                                ObjectFieldDefinitionPosition {
                                    type_name: empty_name.clone(),
                                    field_name: empty_name.clone(),
                                },
                            ),
                            directive_name: empty_name.clone(),
                            directive_index: 0,
                        },
                    }),
                });

                Self {
                    graph,
                    current_source: None,
                    entering_node,
                }
            }

            /// Export the graph as a [dot]() file.
            ///
            /// Note: PetGraph does not support subgraphs, so this method does a few
            /// hacks to manually join the generated dots into one combined file.
            pub fn into_dot(self) -> String {
                // Collect all nodes, grouped by source ID
                let nodes_by_source_id = self
                    .graph
                    .raw_nodes()
                    .iter()
                    .group_by(|node| node.weight.source_id.clone());
                let edges = self.graph.raw_edges();

                // Create graphs for each set of nodes, and immediately export it as a dot file
                let subgraphs =
                    nodes_by_source_id
                        .into_iter()
                        .enumerate()
                        .map(|(index, (id, nodes))| {
                            let subgraph = nodes.fold(
                                Graph::new(),
                                |mut acc: Graph<MockNode, MockEdge>, node| {
                                    acc.add_node(node.weight.clone());
                                    acc
                                },
                            );

                            // Grab the contents of the dot file
                            let dot = Dot::with_config(&subgraph, &[Config::GraphContentOnly])
                                .to_string();

                            // Pair the unique ID of the connect directive with the rendered subgraph, stripping any extra whitespace
                            // for later formatting.
                            //
                            // Note: We also prefix the nodes with their subgraph index so that they are unique within the context
                            // of the entire graph.
                            let SourceId::Connect(connect_id) = id else {
                                unreachable!()
                            };

                            (
                                connect_id,
                                dot.lines()
                                    .map(|line| format!("{index}.{}", line.trim()))
                                    .join("\n"),
                            )
                        });

                // We'll need to keep track of how many nodes are in each subgraph so that we can manually construct the
                // edges after using these counts as a way to caluclate the offset.
                let mut counts = Vec::new();

                // Render the final graph
                // Something like `indoc!` would be super useful here...
                let mut combined_dot = String::new();
                combined_dot.push_str("digraph {\n");

                for (index, (id, dot)) in subgraphs.enumerate() {
                    counts.push(dot.lines().count());

                    combined_dot.extend([
                        &format!("  subgraph cluster_{index} {{\n"),
                        "    node [style = filled,color = white]\n",
                        "\n",
                    ]);
                    combined_dot.extend(dot.lines().map(|line| format!("    {line}\n")));
                    combined_dot.extend([
                        "\n",
                        "    style = filled\n",
                        "    color = lightgrey\n",
                        &format!("    label = \"{id}\"\n"),
                    ]);
                    combined_dot.push_str("  }\n");
                }

                // Map the counts now to be the total running count from the index
                let len = counts.len();
                counts = counts
                    .into_iter()
                    .fold(Vec::with_capacity(len), |mut acc, next| {
                        let last = acc.last().unwrap_or(&0);
                        acc.push(last + next);

                        acc
                    });

                // Helper to get the offset and index from a node ID's index
                let offset_and_index = move |index: usize| -> (usize, usize) {
                    let last_max_pos = counts.iter().rposition(|&count| count <= index);

                    if let Some(last_max) = last_max_pos {
                        (last_max + 1, index - counts[last_max])
                    } else {
                        (0, index)
                    }
                };

                // Add in the edges
                for edge in edges {
                    let (from_offset, from) = offset_and_index(edge.source().index());
                    let (to_offset, to) = offset_and_index(edge.target().index());

                    combined_dot.push_str(&format!(
                        "  {}.{} -> {}.{} [ label = \"{}\" ]\n",
                        from_offset,
                        from,
                        to_offset,
                        to,
                        edge.weight.to_string().replace('"', "\\\"")
                    ));
                }

                // Finish off the graph
                combined_dot.push('}');
                combined_dot
            }
        }

        impl IntraSourceQueryGraphBuilderApi for MockSourceQueryGraphBuilder {
            fn add_and_set_current_source(
                &mut self,
                source: SourceId,
            ) -> Result<(), FederationError> {
                self.current_source = Some(source);

                Ok(())
            }

            // We only support concrete types for now
            fn add_concrete_node(
                &mut self,
                supergraph_type_name: NamedType,
                source_data: SourceFederatedConcreteQueryGraphNode,
            ) -> Result<NodeIndex, FederationError> {
                let SourceFederatedConcreteQueryGraphNode::Connect(_data) = source_data else {
                    unreachable!()
                };

                Ok(self.graph.add_node(MockNode {
                    prefix: "Node".to_string(),
                    type_name: supergraph_type_name,
                    source_id: self.current_source.as_ref().unwrap().clone(),
                }))
            }

            fn add_concrete_field_edge(
                &mut self,
                head: NodeIndex,
                tail: NodeIndex,
                supergraph_field_name: Name,
                _self_conditions: IndexSet<SelfConditionIndex>,
                source_data: SourceFederatedConcreteFieldQueryGraphEdge,
            ) -> Result<EdgeIndex, FederationError> {
                let SourceFederatedConcreteFieldQueryGraphEdge::Connect(data) = source_data else {
                    unreachable!()
                };

                let path = match data {
                    ConnectFederatedConcreteFieldQueryGraphEdge::Selection {
                        property_path,
                        ..
                    } => property_path,
                    _ => Vec::new(),
                };
                Ok(self.graph.add_edge(
                    head,
                    tail,
                    MockEdge {
                        field_name: supergraph_field_name,
                        path,
                    },
                ))
            }

            fn add_scalar_node(
                &mut self,
                supergraph_type_name: NamedType,
                _source_data: SourceFederatedScalarQueryGraphNode,
            ) -> Result<NodeIndex, FederationError> {
                Ok(self.graph.add_node(MockNode {
                    prefix: "Scalar".to_string(),
                    type_name: supergraph_type_name,
                    source_id: self.current_source.as_ref().unwrap().clone(),
                }))
            }

            fn add_source_entering_edge(
                &mut self,
                tail: NodeIndex,
                _self_conditions: Option<SelfConditionIndex>,
                source_data: SourceFederatedSourceEnteringQueryGraphEdge,
            ) -> Result<EdgeIndex, FederationError> {
                let SourceFederatedSourceEnteringQueryGraphEdge::Connect(
                    ConnectFederatedSourceEnteringQueryGraphEdge::ConnectParent { subgraph_type },
                ) = source_data
                else {
                    unreachable!()
                };

                Ok(self.graph.add_edge(
                    self.entering_node,
                    tail,
                    MockEdge {
                        field_name: subgraph_type.type_name,
                        path: Vec::new(),
                    },
                ))
            }

            // ---------------------------------
            // -- Everything below is todo!() --
            // ---------------------------------

            fn source_query_graph(
                &mut self,
            ) -> Result<&mut SourceFederatedQueryGraph, FederationError> {
                todo!()
            }

            fn get_current_source(&self) -> Result<SourceId, FederationError> {
                todo!()
            }

            fn add_self_condition(
                &mut self,
                _supergraph_type_name: NamedType,
                _field_set: &str,
            ) -> Result<SelfConditionIndex, FederationError> {
                todo!()
            }

            fn add_abstract_node(
                &mut self,
                _supergraph_type_name: NamedType,
                _source_data: SourceFederatedAbstractFieldQueryGraphEdge,
            ) -> Result<NodeIndex, FederationError> {
                todo!()
            }

            fn add_enum_node(
                &mut self,
                _supergraph_type_name: NamedType,
                _source_data: SourceFederatedEnumQueryGraphNode,
            ) -> Result<NodeIndex, FederationError> {
                todo!()
            }

            fn add_abstract_field_edge(
                &mut self,
                _head: NodeIndex,
                _tail: NodeIndex,
                _supergraph_field_name: Name,
                _self_conditions: IndexSet<SelfConditionIndex>,
                _source_data: SourceFederatedAbstractFieldQueryGraphEdge,
            ) -> Result<EdgeIndex, FederationError> {
                todo!()
            }

            fn add_type_condition_edge(
                &mut self,
                _head: NodeIndex,
                _tail: NodeIndex,
                _source_data: SourceFederatedTypeConditionQueryGraphEdge,
            ) -> Result<EdgeIndex, FederationError> {
                todo!()
            }

            fn is_for_query_planning(&self) -> bool {
                todo!()
            }
        }
    }
}
