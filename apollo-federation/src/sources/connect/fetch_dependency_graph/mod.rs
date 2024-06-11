use std::sync::Arc;

use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::Node as NodeElement;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::prelude::EdgeIndex;

use crate::error::FederationError;
use crate::query_plan::fetch_dependency_graph_processor::FETCH_COST;
use crate::source_aware::federated_query_graph;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::path_tree;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::source_aware::query_plan::QueryPlanCost;
use crate::sources::connect;
use crate::sources::connect::json_selection::Alias;
use crate::sources::connect::json_selection::JSONSelection;
use crate::sources::connect::json_selection::Key;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::json_selection::PathSelection;
use crate::sources::connect::json_selection::StarSelection;
use crate::sources::connect::json_selection::SubSelection;
use crate::sources::source;
use crate::sources::source::fetch_dependency_graph::FetchDependencyGraphApi;
use crate::sources::source::fetch_dependency_graph::PathApi;
use crate::sources::source::SourceId;

/// A connect-specific dependency graph for fetches.
#[derive(Debug)]
pub(crate) struct FetchDependencyGraph;

impl FetchDependencyGraphApi for FetchDependencyGraph {
    fn edges_that_can_reuse_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        merge_at: &[FetchDataPathElement],
        source_entering_edge: EdgeIndex,
        path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
        source_data: &source::fetch_dependency_graph::Node,
    ) -> Result<Vec<&'path_tree path_tree::ChildKey>, FederationError> {
        // We are within the context of connect, so ensure that's the case
        let source::fetch_dependency_graph::Node::Connect(source_data) = source_data else {
            return Err(FederationError::internal("expected connect node"));
        };

        // If we have distinct merge positions, or if the entering edge is different from the supplied node,
        // then there is nothing in common and thus nothing reusable.
        if source_entering_edge != source_data.source_entering_edge
            || *merge_at != *source_data.merge_at
        {
            return Ok(Vec::new());
        }

        // Start collecting as many reusable portions as possible
        let mut reusable_edges = Vec::new();
        for edge in path_tree_edges {
            // Grab the field from the edge's operation element, shorting out with an error if it
            // isn't present.
            let op_elem = edge
                .operation_element
                .as_ref()
                .ok_or(FederationError::internal(
                    "a child edge must have an operation element in order to reuse a node",
                ))?;
            let OperationPathElement::Field(op_elem) = op_elem.as_ref() else {
                return Err(FederationError::internal(
                    "a child edge's operation element must be a field in order to reuse a node",
                ));
            };

            // If the names differ, then it isn't usable
            let op_data = op_elem.data();
            if op_data.response_name() != source_data.field_response_name {
                continue;
            }

            // If the arguments differ, then we can't reuse the edge
            if op_data.arguments.len() != source_data.field_arguments.len()
                || op_data.arguments.iter().any(|arg| {
                    !matches!(
                        source_data.field_arguments.get(&arg.name),
                        Some(source_arg) if *source_arg == arg.value
                    )
                })
            {
                continue;
            }

            // If we've gotten this far, then we can reuse the edge
            reusable_edges.push(edge);
        }

        Ok(reusable_edges)
    }

    fn add_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: Arc<[FetchDataPathElement]>,
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
        _path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
    ) -> Result<
        (
            source::fetch_dependency_graph::Node,
            Vec<&'path_tree path_tree::ChildKey>,
        ),
        FederationError,
    > {
        todo!()
    }

    fn new_path(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: Arc<[FetchDataPathElement]>,
        source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        // Grab the corresponding source for this edge, making sure that the edge is
        // actually a valid entrypoint.
        let edge_source_id = {
            let graph_edge = query_graph.edge_weight(source_entering_edge)?;

            let federated_query_graph::Edge::SourceEntering { tail_source_id, .. } = graph_edge
            else {
                return Err(FederationError::internal(
                    "a path should start from an entering edge",
                ));
            };

            tail_source_id.clone()
        };

        Ok(Path {
            merge_at,
            source_entering_edge,
            source_id: edge_source_id,
            field: None,
        }
        .into())
    }

    fn add_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        source_path: source::fetch_dependency_graph::Path,
        source_data: &mut source::fetch_dependency_graph::Node,
    ) -> Result<(), FederationError> {
        // Since we are handling connect code, we should make sure that we actually have connect data
        let source::fetch_dependency_graph::Path::Connect(source_path) = source_path else {
            return Err(FederationError::internal("expected connect path"));
        };
        let source::fetch_dependency_graph::Node::Connect(source_data) = source_data else {
            return Err(FederationError::internal("expected connect path"));
        };

        // We should be at the same merge level
        // Note: This should be a fast comparison (pointer-level)
        if source_path.merge_at != source_data.merge_at {
            return Err(FederationError::internal(
                "expected to have matching merge elements",
            ));
        }

        // The given edge should be one that connects to both the source_path and source_data
        // Note: Comparison of two numbers is fast
        if source_path.source_entering_edge != source_data.source_entering_edge {
            return Err(FederationError::internal(
                "expected to have matching entering edges",
            ));
        }

        // If we don't have any field selections in the source_path, then there is nothing to do.
        let Some(source_path_field) = source_path.field else {
            return Ok(());
        };

        // Enforce that the field contains properties shared with the source data
        if source_path_field.response_name != source_data.field_response_name {
            return Err(FederationError::internal(
                "expected path and source data to have the same field name",
            ));
        }
        if source_path_field.arguments != source_data.field_arguments {
            return Err(FederationError::internal(
                "expected path and source data to have the same field arguments",
            ));
        }

        // Ensure that we have a selection, inserting an initial value if not.
        let selection =
            source_data
                .selection
                .get_or_insert_with(|| match &source_path_field.selections {
                    // Construct a new selection from the supplied path properties
                    PathSelections::Selections {
                        head_property_path,
                        tail_selection,
                        ..
                    } => {
                        if head_property_path.is_empty() {
                            JSONSelection::Named(SubSelection::default())
                        } else if let Some((_name, _tail)) = tail_selection {
                            JSONSelection::Path(PathSelection::from_slice(
                                head_property_path,
                                Some(SubSelection::default()),
                            ))
                        } else {
                            JSONSelection::Path(PathSelection::from_slice(head_property_path, None))
                        }
                    }

                    // Pass through the supplied selection
                    PathSelections::CustomScalarRoot { selection } => selection.clone(),
                });

        // TODO: Matching twice seems sad, but how can we separate the selection logic from the traversal?
        // If figured out, remove clone above
        if let PathSelections::Selections {
            named_selections,
            tail_selection,
            ..
        } = source_path_field.selections
        {
            // We can short out if there's nothing to select
            let Some((tail_name, tail_subselection)) = tail_selection else {
                return Ok(());
            };

            // If we are adding a path and have a tail selection, then the selection _must_ have a subselection to account
            // for the extra tail.
            let subselection =
                selection
                    .next_mut_subselection()
                    .ok_or(FederationError::internal(
                        "expecting a subselection in our selection",
                    ))?;

            // Helper method for finding existing references of names within a vec of keys
            fn name_matches(seen_selection: &NamedSelection, name: &Name) -> bool {
                match seen_selection {
                    NamedSelection::Field(Some(Alias { name: ident }), _, _)
                    | NamedSelection::Field(None, ident, _)
                    | NamedSelection::Quoted(Alias { name: ident }, _, _)
                    | NamedSelection::Path(Alias { name: ident }, _)
                    | NamedSelection::Group(Alias { name: ident }, _) => ident == name.as_str(),
                }
            }

            // Now we need to traverse the hierarchy behind the supplied node, updating its JSONSelections
            // along the way as we find missing members needed by the new source path.
            let mut subselection_ref = subselection;
            for (name, keys) in named_selections {
                // If we have a selection already, we'll need to make sure that it includes the new field,
                // then we process the next subselection in the path chain.
                // TODO: This is probably not very performant, but we only have a Vec to work with...
                subselection_ref = if let Some(matching_selection_position) = subselection_ref
                    .selections
                    .iter()
                    .position(|s| name_matches(s, &name))
                {
                    let matching_selection = subselection_ref.selections.get_mut(matching_selection_position).ok_or(FederationError::internal("matched position does not actually exist in selections. This should not happen"))?;
                    matching_selection
                        .next_mut_subselection()
                        .ok_or(FederationError::internal(
                            "expected existing selection to have a subselection",
                        ))?
                } else if keys.is_empty() {
                    subselection_ref.selections.push(NamedSelection::Group(
                        Alias {
                            name: name.to_string(),
                        },
                        SubSelection::default(),
                    ));

                    subselection_ref
                        .selections
                        .last_mut()
                        .ok_or(FederationError::internal(
                            "recently added group named selection disappeared. This should not happen",
                        ))?
                        .next_mut_subselection()
                        .ok_or(FederationError::internal(
                            "recently added group named selection's subselection disappeared. This should not happen",
                        ))?
                } else {
                    // TODO: You could technically detect whether a shorthand enum variant of NamedSelection
                    // is usable based on the Name and Keys to make the overall JSONSelection appear cleaner,
                    // though this isn't necessary.
                    subselection_ref.selections.push(NamedSelection::Path(
                        Alias {
                            name: name.to_string(),
                        },
                        PathSelection::from_slice(&keys, Some(SubSelection::default())),
                    ));

                    subselection_ref
                        .selections
                        .last_mut()
                        .ok_or(FederationError::internal(
                            "recently added path named selection disappeared. This should not happen",
                        ))?
                        .next_mut_subselection()
                        .ok_or(FederationError::internal(
                            "recently added path named selection's subselection disappeared. This should not happen",
                        ))?
                };
            }

            // Now that we've merged in the JSON selection into the node, add in the final tail subselection
            // Note: The subselection_ref here is now the furthest down in the chain, which is where we need
            // it to be.
            match tail_subselection {
                // TODO: This is probably not very performant, but we only have a Vec to work with...
                PathTailSelection::Selection { property_path } => {
                    if !subselection_ref
                        .selections
                        .iter()
                        .any(|s| name_matches(s, &tail_name))
                    {
                        subselection_ref.selections.push(NamedSelection::Path(
                            Alias {
                                name: tail_name.to_string(),
                            },
                            PathSelection::from_slice(&property_path, None),
                        ));
                    }
                }
                PathTailSelection::CustomScalarPathSelection { path_selection } => {
                    if !subselection_ref
                        .selections
                        .iter()
                        .any(|s| name_matches(s, &tail_name))
                    {
                        subselection_ref.selections.push(NamedSelection::Path(
                            Alias {
                                name: tail_name.to_string(),
                            },
                            path_selection,
                        ));
                    }
                }

                PathTailSelection::CustomScalarStarSelection {
                    star_subselection,
                    excluded_properties,
                } => {
                    if subselection_ref.star.is_none() {
                        // Initialize the star
                        subselection_ref.star = Some(StarSelection(
                            Some(Alias {
                                name: tail_name.to_string(),
                            }),
                            star_subselection.map(Box::new),
                        ));

                        // Keep track of which props we've excluded
                        for (index, key) in excluded_properties.into_iter().enumerate() {
                            let alias = format!("____excluded_star_key__{index}");
                            subselection_ref.selections.push(NamedSelection::Quoted(
                                Alias { name: alias },
                                key.as_string(),
                                None,
                            ));
                        }
                    }
                }
            };
        }

        Ok(())
    }

    fn to_cost(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &source::fetch_dependency_graph::Node,
    ) -> Result<QueryPlanCost, FederationError> {
        // REST doesn't let you (normally) select only a subset of the response,
        // so the cost is constant regardless of what was selected.
        Ok(FETCH_COST)
    }

    fn to_plan_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &source::fetch_dependency_graph::Node,
        _fetch_count: u32,
    ) -> Result<source::query_plan::FetchNode, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) struct Node {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    field_response_name: Name,
    field_arguments: IndexMap<Name, NodeElement<Value>>,
    selection: Option<JSONSelection>,
}

/// Connect-specific path tracking information.
///
/// A [Path] describes tracking information useful when doing introspection
/// of a connect-specific query.
#[derive(Debug, Clone)]
pub(crate) struct Path {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    source_id: SourceId,
    field: Option<PathField>,
}

#[derive(Debug, Clone)]
pub(crate) struct PathField {
    response_name: Name,
    arguments: IndexMap<Name, NodeElement<Value>>,
    selections: PathSelections,
}

/// A path to a specific selection
///
/// This enum encompasses a set of directions for reaching a target property
/// within a JSON selection.
#[derive(Debug, Clone)]
pub(crate) enum PathSelections {
    /// Set of selections assuming a starting point of the root.
    Selections {
        /// Property path from the head
        ///
        /// This is a list of properties to traverse starting from the root (or head)
        /// of the corresponding selection. These should be simple paths that can be
        /// chained together.
        head_property_path: Vec<Key>,

        /// Named selections from the root reachable through [head_property_path]
        ///
        /// Each member in this list is of the form ([Name], [Keys](Vec<Key>)) and is
        /// a chain of named selections to apply iteratively to reach the leaf of our selection.
        ///
        /// Note: [Name] here can refer to aliased fields as well.
        named_selections: Vec<(Name, Vec<Key>)>,

        /// The (optional) final selection for this chain.
        ///
        /// This selection is assumed to be from the context of the node accessable from
        /// the chain of [head_property_path] followed by the chain of [named_selections].
        ///
        /// A value of `None` here means to stop traversal at the current selection, while any
        /// other value signals that there might be further sections to traverse.
        tail_selection: Option<(Name, PathTailSelection)>,
    },

    /// The full selection from a (potentially) different root
    CustomScalarRoot {
        /// The full selection
        selection: JSONSelection,
    },
}

/// A path to a specific selection, not from the root.
///
/// This enum describes different ways to perform a final selection of a path
/// from the context of any selection in the tree.
#[derive(Debug, Clone)]
pub(crate) enum PathTailSelection {
    /// Simple selection using a chain of keys.
    Selection {
        /// The chain of [Key]s to traverse
        property_path: Vec<Key>,
    },

    /// Custom selection using a [PathSelection]
    ///
    /// Note: This is useful when a simple [PathTailSelection::Selection] is not
    /// complex enough to describe the traversal path, such as when using variables
    /// or custom [SubSelection]s.
    CustomScalarPathSelection { path_selection: PathSelection },

    /// Custom selection using a star (*) subselection.
    ///
    /// Note: This is useful when needing to collect all other possible values
    /// in a selection into a singular property.
    CustomScalarStarSelection {
        /// The subselection including the star
        star_subselection: Option<SubSelection>,

        /// All other known properties that _shouldn't_ be collected into the
        /// star selection.
        excluded_properties: IndexSet<Key>,
    },
}

impl PathApi for Path {
    fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    fn add_operation_element(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        operation_element: Arc<OperationPathElement>,
        edge: Option<EdgeIndex>,
        _self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        // For this milestone, we only allow `NormalizedField`s for operation elements
        let OperationPathElement::Field(operation_field) = operation_element.as_ref() else {
            return Err(FederationError::internal(
                "operation elements must be called on a field",
            ));
        };

        // For milestone 1, we don't consider cases where the edge is not present
        let Some(edge) = edge else {
            return Err(FederationError::internal("edge cannot be None"));
        };

        // Extract the edge information for this operation
        let federated_query_graph::Edge::ConcreteField { source_data, .. } =
            query_graph.edge_weight(edge)?
        else {
            return Err(FederationError::internal(
                "operation elements should only be called for concrete fields",
            ));
        };

        let source::federated_query_graph::ConcreteFieldEdge::Connect(concrete_field_edge) =
            source_data
        else {
            return Err(FederationError::internal(
                "operation element's source data must be a connect concrete field",
            ));
        };

        // We need to figure out now what path to take based on what needs updating in the original connect path
        let connect_field = self
            .field
            .to_owned()
            .map(|field| {
                // Deconstruct the original selection
                let PathSelections::Selections {
                    head_property_path,
                    named_selections,
                    tail_selection: None,
                } = field.selections
                else {
                    return Err(FederationError::internal(
                        "expected the existing field to have selections with no tail",
                    ));
                };

                // Recreate it with additional info
                let selections = match concrete_field_edge {
                    connect::federated_query_graph::ConcreteFieldEdge::Selection {
                        property_path,
                        ..
                    } => {
                        let (_, operation_target_index) = query_graph.edge_endpoints(edge)?;
                        let operation_target_node =
                            query_graph.node_weight(operation_target_index)?;

                        match operation_target_node {
                            federated_query_graph::Node::Concrete { .. } => {
                                let concrete_selection = (
                                    operation_field.data().response_name(),
                                    property_path.clone(),
                                );

                                let mut named_selections = named_selections;
                                named_selections.push(concrete_selection);
                                PathSelections::Selections {
                                    head_property_path,
                                    named_selections,
                                    tail_selection: None,
                                }
                            }

                            federated_query_graph::Node::Enum { .. }
                            | federated_query_graph::Node::Scalar { .. } => {
                                let new_tail = PathTailSelection::Selection {
                                    property_path: property_path.clone(),
                                };

                                PathSelections::Selections {
                                    head_property_path,
                                    named_selections,
                                    tail_selection: Some((operation_field.data().response_name(), new_tail)),
                                }
                            }

                            other => return Err(FederationError::internal(format!("expected the tail edge to contain a concrete, enum, or scalar node, found: {other:?}"))),
                        }
                    }

                    connect::federated_query_graph::ConcreteFieldEdge::CustomScalarPathSelection {
                        path_selection,
                        ..
                    } => {
                        let new_tail = PathTailSelection::CustomScalarPathSelection {
                            path_selection: path_selection.clone(),
                        };

                        PathSelections::Selections {
                            head_property_path,
                            named_selections,
                            tail_selection: Some((operation_field.data().response_name(), new_tail)),
                        }
                    }

                    connect::federated_query_graph::ConcreteFieldEdge::CustomScalarStarSelection {
                        star_subselection,
                        excluded_properties,
                        ..
                    } => {
                        let new_tail = PathTailSelection::CustomScalarStarSelection {
                            star_subselection: star_subselection.clone(),
                            excluded_properties: excluded_properties.clone(),
                        };

                        PathSelections::Selections {
                            head_property_path,
                            named_selections,
                            tail_selection: Some((operation_field.data().response_name(), new_tail)),
                        }
                    }

                    _ => {
                        return Err(FederationError::internal(
                            "expected the concrete edge to be a selection",
                        ))
                    }
                };

                Ok(PathField { response_name: field.response_name, arguments: field.arguments, selections })
            })
            .unwrap_or_else(|| {
                let connect::federated_query_graph::ConcreteFieldEdge::Connect {
                    subgraph_field: _subgraph_field,
                } = concrete_field_edge
                else {
                    return Err(FederationError::internal(
                        "expected the field edge to be connect",
                    ));
                };

                let (_, operation_target_index) = query_graph.edge_endpoints(edge)?;
                let operation_target_node = query_graph.node_weight(operation_target_index)?;
                let selections = match operation_target_node {
                    federated_query_graph::Node::Concrete {
                        source_data:
                            source::federated_query_graph::ConcreteNode::Connect(
                                connect::federated_query_graph::ConcreteNode::SelectionRoot {
                                    property_path,
                                    ..
                                },
                            ),
                        ..
                    }
                    | federated_query_graph::Node::Enum {
                        source_data:
                            source::federated_query_graph::EnumNode::Connect(
                                connect::federated_query_graph::EnumNode::SelectionRoot {
                                    property_path,
                                    ..
                                },
                            ),
                        ..
                    }
                    | federated_query_graph::Node::Scalar {
                        source_data:
                            source::federated_query_graph::ScalarNode::Connect(
                                connect::federated_query_graph::ScalarNode::SelectionRoot {
                                    property_path,
                                    ..
                                },
                            ),
                        ..
                    } => PathSelections::Selections {
                        head_property_path: property_path.clone(),
                        named_selections: Vec::new(),
                        tail_selection: None,
                    },

                    federated_query_graph::Node::Scalar {
                        source_data:
                            source::federated_query_graph::ScalarNode::Connect(
                                connect::federated_query_graph::ScalarNode::CustomScalarSelectionRoot {
                                    selection,
                                    ..
                                },
                            ),
                        ..
                    } => PathSelections::CustomScalarRoot {
                        selection: selection.clone(),
                    },

                    _ => {
                        return Err(FederationError::internal(
                            "expected a concrete type, enum, or scalar",
                        ))
                    }
                };

                Ok(PathField {
                    response_name: operation_field.data().response_name(),
                    arguments: operation_field
                        .data()
                        .arguments
                        .iter()
                        .map(|arg| (arg.name.clone(), arg.value.clone()))
                        .collect(),
                    selections,
                })
            })?;

        Ok(source::fetch_dependency_graph::Path::Connect(Path {
            merge_at: self.merge_at.clone(),
            source_entering_edge: self.source_entering_edge,
            source_id: self.source_id.clone(),
            field: Some(connect_field),
        }))
    }
}

#[cfg(test)]
mod tests {
    mod fetch {
        use std::sync::Arc;

        use apollo_compiler::ast::Name;
        use apollo_compiler::executable;
        use apollo_compiler::name;
        use apollo_compiler::Schema;
        use indexmap::IndexMap;
        use insta::assert_debug_snapshot;
        use insta::assert_snapshot;
        use itertools::Itertools;
        use petgraph::graph::DiGraph;
        use petgraph::prelude::EdgeIndex;

        use crate::operation::Field;
        use crate::operation::FieldData;
        use crate::schema::position::FieldDefinitionPosition;
        use crate::schema::position::ObjectFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;
        use crate::schema::ValidFederationSchema;
        use crate::source_aware::federated_query_graph;
        use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
        use crate::source_aware::federated_query_graph::path_tree::ChildKey;
        use crate::source_aware::federated_query_graph::FederatedQueryGraph;
        use crate::source_aware::query_plan::FetchDataPathElement;
        use crate::sources::connect;
        use crate::sources::connect::federated_query_graph::ConcreteFieldEdge;
        use crate::sources::connect::federated_query_graph::ConcreteNode;
        use crate::sources::connect::federated_query_graph::SourceEnteringEdge;
        use crate::sources::connect::fetch_dependency_graph::FetchDependencyGraph;
        use crate::sources::connect::json_selection::Alias;
        use crate::sources::connect::json_selection::Key;
        use crate::sources::connect::json_selection::NamedSelection;
        use crate::sources::connect::json_selection::PrettyPrintable;
        use crate::sources::connect::ConnectId;
        use crate::sources::connect::JSONSelection;
        use crate::sources::source;
        use crate::sources::source::fetch_dependency_graph::FetchDependencyGraphApi;
        use crate::sources::source::SourceId;

        struct SetupInfo {
            fetch_graph: FetchDependencyGraph,
            query_graph: Arc<FederatedQueryGraph>,
            source_id: SourceId,
            source_entry_edges: Vec<EdgeIndex>,
            non_source_entry_edges: Vec<EdgeIndex>,
            schema: ValidFederationSchema,
        }
        fn setup() -> SetupInfo {
            let mut graph = DiGraph::new();

            // Fill in some dummy data
            // Root
            // |- Post (entry edge)
            // |- User (entry edge)
            // |- View
            let source_id = SourceId::Connect(ConnectId {
                label: "test connect".to_string(),
                subgraph_name: "CONNECT".into(),
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                        ObjectFieldDefinitionPosition {
                            type_name: name!("TestObject"),
                            field_name: name!("testField"),
                        },
                    ),
                    directive_name: name!("connect"),
                    directive_index: 0,
                },
            });

            // Create a root
            let query = graph.add_node(federated_query_graph::Node::Concrete {
                supergraph_type: ObjectTypeDefinitionPosition {
                    type_name: name!("Query"),
                },
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: ConcreteNode::SelectionRoot {
                    subgraph_type: ObjectTypeDefinitionPosition {
                        type_name: name!("Query"),
                    },
                    property_path: Vec::new(),
                }
                .into(),
            });

            // Make the nodes with entrypoints
            let mut edges = Vec::new();
            let entrypoints = 2;
            for (index, type_name) in ["Post", "User", "View"].into_iter().enumerate() {
                let node_type = ObjectTypeDefinitionPosition {
                    type_name: Name::new(type_name).unwrap(),
                };

                let node = graph.add_node(federated_query_graph::Node::Concrete {
                    supergraph_type: node_type.clone(),
                    field_edges: IndexMap::new(),
                    source_exiting_edge: None,
                    source_id: source_id.clone(),
                    source_data: ConcreteNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field(type_name.to_lowercase().to_string())],
                    }
                    .into(),
                });

                let field = ObjectFieldDefinitionPosition {
                    type_name: Name::new(type_name).unwrap(),
                    field_name: Name::new(type_name.to_lowercase()).unwrap(),
                };
                edges.push(
                    graph.add_edge(
                        query,
                        node,
                        federated_query_graph::Edge::ConcreteField {
                            supergraph_field: field.clone(),
                            self_conditions: Default::default(),
                            source_id: source_id.clone(),
                            source_data: ConcreteFieldEdge::Connect {
                                subgraph_field: field.clone(),
                            }
                            .into(),
                        },
                    ),
                );

                // Optionally add the entrypoint
                if index < entrypoints {
                    edges.push(
                        graph.add_edge(
                            query,
                            node,
                            federated_query_graph::Edge::SourceEntering {
                                supergraph_type: node_type.clone(),
                                self_conditions: Default::default(),
                                tail_source_id: source_id.clone(),
                                source_data: SourceEnteringEdge::ConnectParent {
                                    subgraph_type: node_type,
                                }
                                .into(),
                            },
                        ),
                    );
                }
            }

            let (entry, non_entry) = edges.into_iter().partition(|&edge_index| {
                matches!(
                    graph.edge_weight(edge_index),
                    Some(federated_query_graph::Edge::SourceEntering { .. })
                )
            });

            // Create a dummy schema for tests
            let schema =
                Schema::parse(include_str!("../tests/schemas/simple.graphql"), "").unwrap();
            let schema = schema.validate().unwrap();
            let schema = ValidFederationSchema::new(schema).unwrap();

            SetupInfo {
                fetch_graph: FetchDependencyGraph,
                query_graph: Arc::new(FederatedQueryGraph::with_graph(graph)),
                source_id,
                source_entry_edges: entry,
                non_source_entry_edges: non_entry,
                schema,
            }
        }

        #[test]
        fn it_handles_a_new_path() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                ..
            } = setup();

            // Make sure that the first edge is what we expect
            let last_edge_index = *source_entry_edges.last().unwrap();
            let (query_root_index, post_index) =
                query_graph.edge_endpoints(last_edge_index).unwrap();
            assert_debug_snapshot!(query_graph.node_weight(query_root_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(Query),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(Query),
                    property_path: [],
                },
            ),
        }
        "###);
            assert_debug_snapshot!(query_graph.node_weight(post_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(User),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(User),
                    property_path: [
                        Field(
                            "user",
                        ),
                    ],
                },
            ),
        }
        "###);

            let path = fetch_graph
                .new_path(query_graph, Arc::new([]), last_edge_index, None)
                .unwrap();

            assert_debug_snapshot!(
                path,
                @r###"
        Connect(
            Path {
                merge_at: [],
                source_entering_edge: EdgeIndex(3),
                source_id: Connect(
                    ConnectId {
                        label: "test connect",
                        subgraph_name: "CONNECT",
                        directive: ObjectOrInterfaceFieldDirectivePosition {
                            field: Object(TestObject.testField),
                            directive_name: "connect",
                            directive_index: 0,
                        },
                    },
                ),
                field: None,
            },
        )
        "###
            );
        }

        #[test]
        fn it_fails_with_invalid_entrypoint() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                non_source_entry_edges,
                ..
            } = setup();

            // Make sure that the first edge is what we expect
            let last_edge_index = *non_source_entry_edges.last().unwrap();
            let (query_root_index, view_index) =
                query_graph.edge_endpoints(last_edge_index).unwrap();
            assert_debug_snapshot!(query_graph.node_weight(query_root_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(Query),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(Query),
                    property_path: [],
                },
            ),
        }
        "###);
            assert_debug_snapshot!(query_graph.node_weight(view_index).unwrap(), @r###"
        Concrete {
            supergraph_type: Object(View),
            field_edges: {},
            source_exiting_edge: None,
            source_id: Connect(
                ConnectId {
                    label: "test connect",
                    subgraph_name: "CONNECT",
                    directive: ObjectOrInterfaceFieldDirectivePosition {
                        field: Object(TestObject.testField),
                        directive_name: "connect",
                        directive_index: 0,
                    },
                },
            ),
            source_data: Connect(
                SelectionRoot {
                    subgraph_type: Object(View),
                    property_path: [
                        Field(
                            "view",
                        ),
                    ],
                },
            ),
        }
        "###);

            // Make sure that we fail since we do not have an entering edge
            let path = fetch_graph.new_path(query_graph, Arc::new([]), last_edge_index, None);

            let Err(path) = path else {
                panic!("Unexpectedly succeeded with non-source-entering edge.")
            };
            assert_snapshot!(
                path,
                @r###"
            An internal error has occurred, please report this bug to Apollo.

            Details: a path should start from an entering edge
            "###
            );
        }

        #[test]
        fn it_fails_with_invalid_edge() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                ..
            } = setup();

            // Make sure that the first edge is what we expect
            let invalid_index = EdgeIndex::end();

            // Make sure that we fail since we pass in an invalid edge
            let path = fetch_graph.new_path(query_graph, Arc::new([]), invalid_index, None);

            let Err(path) = path else {
                panic!("Unexpectedly succeeded with invalid edge.")
            };
            assert_snapshot!(
                path,
                @r###"
            An internal error has occurred, please report this bug to Apollo.

            Details: Edge unexpectedly missing
            "###
            );
        }

        /// Tests adding in a new path.
        ///
        /// This test ensures that nodes which have no existing JSONSelection will correctly
        /// have the new additions merged in from a separate path.
        ///
        /// - Node's selection: _
        /// - Path's selection: { a b c }
        #[test]
        fn it_adds_a_simple_path() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                source_id,
                ..
            } = setup();

            let arguments = IndexMap::from([]);
            let merge_at = Arc::new([]);
            let source_entering_edge = *source_entry_edges.last().unwrap();
            let response_name = name!("_simple_path_test");

            let (unmatched, selection) = JSONSelection::parse("a b c").unwrap();
            assert!(unmatched.is_empty());

            let mut node = source::fetch_dependency_graph::Node::Connect(
                connect::fetch_dependency_graph::Node {
                    merge_at: merge_at.clone(),
                    source_entering_edge,
                    field_response_name: response_name.clone(),
                    field_arguments: arguments.clone(),
                    selection: Some(selection),
                },
            );

            fetch_graph
                .add_path(
                    query_graph,
                    source::fetch_dependency_graph::Path::Connect(
                        connect::fetch_dependency_graph::Path {
                            merge_at,
                            source_entering_edge,
                            source_id,
                            field: Some(connect::fetch_dependency_graph::PathField {
                                response_name,
                                arguments,
                                selections:
                                    connect::fetch_dependency_graph::PathSelections::Selections {
                                        head_property_path: Vec::new(),
                                        named_selections: Vec::new(),
                                        tail_selection: None,
                                    },
                            }),
                        },
                    ),
                    &mut node,
                )
                .unwrap();

            let source::fetch_dependency_graph::Node::Connect(result) = node else {
                unreachable!()
            };
            assert_eq!(*result.merge_at, []);
            assert_eq!(result.source_entering_edge, source_entering_edge);
            assert_eq!(result.field_response_name.as_str(), "_simple_path_test");
            assert_eq!(result.field_arguments, IndexMap::new());
            assert_snapshot!(result.selection.unwrap().pretty_print(), @r###"
            {
              a
              b
              c
            }
            "###);
        }

        /// Tests adding in a new nested path.
        ///
        /// This test ensures that nodes which have no existing JSONSelection will correctly
        /// have the new additions merged in from a separate path, including nesting.
        ///
        /// - Node's selection: _
        /// - Path's selection:
        /// {
        ///   a
        ///   b {
        ///     x
        ///     y
        ///     z {
        ///       one
        ///       two
        ///       three
        ///     }
        ///   }
        ///   c: last
        /// }
        #[test]
        fn it_adds_a_nested_path() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                source_id,
                ..
            } = setup();

            let arguments = IndexMap::from([]);
            let merge_at = Arc::new([]);
            let source_entering_edge = *source_entry_edges.last().unwrap();
            let response_name = name!("_nested_path_test");

            let (unmatched, selection) =
                JSONSelection::parse("a b { x y z { one two three } } c: last").unwrap();
            assert!(unmatched.is_empty());

            let mut node = source::fetch_dependency_graph::Node::Connect(
                connect::fetch_dependency_graph::Node {
                    merge_at: merge_at.clone(),
                    source_entering_edge,
                    field_response_name: response_name.clone(),
                    field_arguments: arguments.clone(),
                    selection: Some(selection),
                },
            );

            fetch_graph
                .add_path(
                    query_graph,
                    source::fetch_dependency_graph::Path::Connect(
                        connect::fetch_dependency_graph::Path {
                            merge_at,
                            source_entering_edge,
                            source_id,
                            field: Some(connect::fetch_dependency_graph::PathField {
                                response_name,
                                arguments,
                                selections:
                                    connect::fetch_dependency_graph::PathSelections::Selections {
                                        head_property_path: Vec::new(),
                                        named_selections: Vec::new(),
                                        tail_selection: None,
                                    },
                            }),
                        },
                    ),
                    &mut node,
                )
                .unwrap();

            let source::fetch_dependency_graph::Node::Connect(result) = node else {
                unreachable!()
            };
            assert_eq!(*result.merge_at, []);
            assert_eq!(result.source_entering_edge, source_entering_edge);
            assert_eq!(result.field_response_name.as_str(), "_nested_path_test");
            assert_eq!(result.field_arguments, IndexMap::new());
            assert_snapshot!(result.selection.unwrap().pretty_print(), @r###"
            {
              a
              b {
                x
                y
                z {
                  one
                  two
                  three
                }
              }
              c: last
            }
            "###);
        }

        /// Tests merging in of a new path.
        ///
        /// This test ensures that nodes which already contain a portion of the new path
        /// will correctly have the new additions merged in from a separate path, including
        /// nesting.
        ///
        /// - Node's selection:
        /// .foo.bar {
        ///   qux: .qaax
        ///   qax: .qaax {
        ///     baz
        ///   }
        /// }
        ///
        /// - Path's selection:
        /// .foo.bar {
        ///   qax: .qaax {
        ///     baaz: .baz.buzz {
        ///       biz: .blah {
        ///         x
        ///         y
        ///       }
        ///     }
        ///   }
        /// }
        #[test]
        fn it_merges_a_nested_path() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                source_id,
                ..
            } = setup();

            let arguments = IndexMap::from([]);
            let merge_at = Arc::new([]);
            let source_entering_edge = *source_entry_edges.last().unwrap();
            let response_name = name!("_merge_nested_path_test");

            let (unmatched, selection) = JSONSelection::parse(
                ".foo.bar {
                  qux: .qaax
                  qax: .qaax {
                    baz
                  }
                }",
            )
            .unwrap();
            assert!(unmatched.is_empty());

            let mut node = source::fetch_dependency_graph::Node::Connect(
                connect::fetch_dependency_graph::Node {
                    merge_at: merge_at.clone(),
                    source_entering_edge,
                    field_response_name: response_name.clone(),
                    field_arguments: arguments.clone(),
                    selection: Some(selection),
                },
            );

            fetch_graph
                .add_path(
                    query_graph,
                    source::fetch_dependency_graph::Path::Connect(
                        connect::fetch_dependency_graph::Path {
                            merge_at,
                            source_entering_edge,
                            source_id,
                            field: Some(connect::fetch_dependency_graph::PathField {
                                response_name,
                                arguments,
                                selections:
                                    connect::fetch_dependency_graph::PathSelections::Selections {
                                        head_property_path: vec![
                                            Key::Field("foo".to_string()),
                                            Key::Field("bar".to_string()),
                                        ],
                                        named_selections: vec![
                                            (name!("qax"), vec![Key::Field("qaax".to_string())]),
                                            (
                                                name!("baaz"),
                                                vec![
                                                    Key::Field("baz".to_string()),
                                                    Key::Field("buzz".to_string()),
                                                ],
                                            ),
                                        ],
                                        tail_selection: Some((
                                            name!("biz"),
                                            connect::fetch_dependency_graph::PathTailSelection::CustomScalarPathSelection {
                                                path_selection: connect::fetch_dependency_graph::PathSelection::Selection(connect::SubSelection {
                                                    selections: vec![
                                                        NamedSelection::Group(
                                                            Alias { name: "blah".to_string() },
                                                            connect::SubSelection {
                                                                selections: vec![
                                                                    NamedSelection::Field(None, "x".to_string(), None),
                                                                    NamedSelection::Field(None, "y".to_string(), None)
                                                                ],
                                                                star: None
                                                            })
                                                    ],
                                                    star: None
                                                })
                                            },
                                        )),
                                    },
                            }),
                        },
                    ),
                    &mut node,
                )
                .unwrap();

            let source::fetch_dependency_graph::Node::Connect(result) = node else {
                unreachable!()
            };

            assert_eq!(*result.merge_at, []);
            assert_eq!(result.source_entering_edge, source_entering_edge);
            assert_eq!(
                result.field_response_name.as_str(),
                "_merge_nested_path_test"
            );
            assert_eq!(result.field_arguments, IndexMap::new());
            assert_snapshot!(result.selection.unwrap().pretty_print(), @r###"
            .foo.bar {
              qux: .qaax
              qax: .qaax {
                baz
                baaz: .baz.buzz {
                  biz: {
                    blah: {
                      x
                      y
                    }
                  }
                }
              }
            }
            "###);
        }

        #[test]
        fn it_can_reuse_some_edges() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                schema,
                ..
            } = setup();

            let args = Arc::new(vec![apollo_compiler::Node::new(
                apollo_compiler::ast::Argument {
                    name: name!("single_arg"),
                    value: apollo_compiler::Node::new(apollo_compiler::ast::Value::String(
                        "arg_value".into(),
                    )),
                },
            )]);

            let field_response_name = name!("_matching_name");
            let last_edge_index = *source_entry_edges.last().unwrap();
            let merge_at = [];
            let source_data = source::fetch_dependency_graph::Node::Connect(
                connect::fetch_dependency_graph::Node {
                    merge_at: Arc::new(merge_at.clone()),
                    source_entering_edge: last_edge_index,
                    field_response_name: field_response_name.clone(),
                    field_arguments: IndexMap::from_iter(
                        args.iter()
                            .map(|node| (node.name.clone(), node.value.clone())),
                    ),
                    selection: None,
                },
            );

            // Generate edges that match on even indices, but not index 2
            let edges = Vec::from_iter((0..5).map(|index| ChildKey {
                operation_element: Some(Arc::new(OperationPathElement::Field(Field::new(
                    FieldData {
                        schema: schema.clone(),
                        field_position: FieldDefinitionPosition::Object(
                            ObjectFieldDefinitionPosition {
                                type_name: name!("_test_type"),
                                field_name: name!("_test_field"),
                            },
                        ),
                        alias: Some(if index % 2 == 0 {
                            field_response_name.clone()
                        } else {
                            name!("_non_matching")
                        }),
                        arguments: if index != 2 {
                            args.clone()
                        } else {
                            Arc::new(vec![apollo_compiler::Node::new(
                                apollo_compiler::ast::Argument {
                                    name: name!("single_arg_modified"),
                                    value: apollo_compiler::Node::new(
                                        apollo_compiler::ast::Value::String("arg_value".into()),
                                    ),
                                },
                            )])
                        },
                        directives: Arc::new(executable::DirectiveList::new()),
                        sibling_typename: None,
                    },
                )))),
                edge: Some(EdgeIndex::new(index)),
            }));
            let edges = edges.iter().collect_vec();

            let reusable_edges = fetch_graph
                .edges_that_can_reuse_node(
                    query_graph,
                    &merge_at,
                    last_edge_index,
                    edges,
                    &source_data,
                )
                .unwrap();

            assert_debug_snapshot!(reusable_edges.into_iter().map(|edge| edge.edge.unwrap()).collect_vec(), @r###"
            [
                EdgeIndex(0),
                EdgeIndex(4),
            ]
            "###);
        }

        #[test]
        fn it_does_not_reuse_non_related_edges() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                schema,
                ..
            } = setup();

            let args = Arc::new(vec![]);
            let field_response_name = name!("User");
            let last_edge_index = *source_entry_edges.last().unwrap();
            let source_data = source::fetch_dependency_graph::Node::Connect(
                connect::fetch_dependency_graph::Node {
                    merge_at: Arc::new([]),
                    source_entering_edge: last_edge_index,
                    field_response_name: field_response_name.clone(),
                    field_arguments: IndexMap::new(),
                    selection: None,
                },
            );

            // Generate edges that would match, if not for the preconditions
            let edges = Vec::from_iter((0..5).map(|index| ChildKey {
                operation_element: Some(Arc::new(OperationPathElement::Field(Field::new(
                    FieldData {
                        schema: schema.clone(),
                        field_position: FieldDefinitionPosition::Object(
                            ObjectFieldDefinitionPosition {
                                type_name: name!("_test_type"),
                                field_name: name!("_test_field"),
                            },
                        ),
                        alias: Some(field_response_name.clone()),
                        arguments: args.clone(),
                        directives: Arc::new(executable::DirectiveList::new()),
                        sibling_typename: None,
                    },
                )))),
                edge: Some(EdgeIndex::new(index)),
            }));
            let edges = edges.iter().collect_vec();

            // Unrelated edges shouldn't be reusable
            assert!(
                fetch_graph
                    .edges_that_can_reuse_node(
                        query_graph.clone(),
                        &[],
                        EdgeIndex::end(),
                        edges.clone(),
                        &source_data,
                    )
                    .unwrap()
                    .is_empty(),
                "edge index mismatch should not reuse nodes"
            );

            // merge_at should match between the two
            assert!(
                fetch_graph
                    .edges_that_can_reuse_node(
                        query_graph,
                        &[FetchDataPathElement::AnyIndex],
                        EdgeIndex::end(),
                        edges,
                        &source_data,
                    )
                    .unwrap()
                    .is_empty(),
                "merge_at mismatch should not reuse nodes"
            );
        }

        #[test]
        fn it_does_not_reuse_non_matching_edges() {
            let SetupInfo {
                fetch_graph,
                query_graph,
                source_entry_edges,
                schema,
                ..
            } = setup();

            let args = Arc::new(Vec::new());
            let field_response_name = name!("User");
            let last_edge_index = *source_entry_edges.last().unwrap();
            let source_data = source::fetch_dependency_graph::Node::Connect(
                connect::fetch_dependency_graph::Node {
                    merge_at: Arc::new([]),
                    source_entering_edge: last_edge_index,
                    field_response_name: field_response_name.clone(),
                    field_arguments: IndexMap::new(),
                    selection: None,
                },
            );

            // Generate edges that won't match due to non-related names
            let edges = Vec::from_iter((0..5).map(|index| ChildKey {
                operation_element: Some(Arc::new(OperationPathElement::Field(Field::new(
                    FieldData {
                        schema: schema.clone(),
                        field_position: FieldDefinitionPosition::Object(
                            ObjectFieldDefinitionPosition {
                                type_name: name!("_test_type"),
                                field_name: name!("_test_field"),
                            },
                        ),
                        alias: Some(name!("non_matching_name")),
                        arguments: args.clone(),
                        directives: Arc::new(executable::DirectiveList::new()),
                        sibling_typename: None,
                    },
                )))),
                edge: Some(EdgeIndex::new(index)),
            }));
            let edges = edges.iter().collect_vec();

            assert!(
                fetch_graph
                    .edges_that_can_reuse_node(
                        query_graph.clone(),
                        &[],
                        EdgeIndex::end(),
                        edges.clone(),
                        &source_data,
                    )
                    .unwrap()
                    .is_empty(),
                "non-matching response names should not be reused"
            );
        }
    }

    mod path {
        use std::sync::Arc;

        use apollo_compiler::ast::DirectiveList;
        use apollo_compiler::name;
        use apollo_compiler::Schema;
        use indexmap::IndexMap;
        use insta::assert_debug_snapshot;
        use petgraph::graph::DiGraph;
        use petgraph::prelude::EdgeIndex;
        use petgraph::prelude::NodeIndex;

        use crate::operation::Field;
        use crate::operation::FieldData;
        use crate::schema::position::EnumTypeDefinitionPosition;
        use crate::schema::position::FieldDefinitionPosition;
        use crate::schema::position::ObjectFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
        use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;
        use crate::schema::position::ScalarTypeDefinitionPosition;
        use crate::schema::ValidFederationSchema;
        use crate::source_aware::federated_query_graph;
        use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
        use crate::source_aware::federated_query_graph::FederatedQueryGraph;
        use crate::sources::connect;
        use crate::sources::connect::json_selection::Key;
        use crate::sources::connect::ConnectId;
        use crate::sources::connect::JSONSelection;
        use crate::sources::source;
        use crate::sources::source::fetch_dependency_graph::PathApi;
        use crate::sources::source::SourceId;

        struct SetupInfo {
            graph: DiGraph<federated_query_graph::Node, federated_query_graph::Edge>,
            schema: ValidFederationSchema,
            source_id: SourceId,
        }
        fn setup() -> SetupInfo {
            let mut graph = DiGraph::new();
            let source_id = SourceId::Connect(ConnectId {
                label: "test connect".to_string(),
                subgraph_name: "CONNECT".into(),
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                        ObjectFieldDefinitionPosition {
                            type_name: name!("_testObject"),
                            field_name: name!("testField"),
                        },
                    ),
                    directive_name: name!("connect"),
                    directive_index: 0,
                },
            });

            // Create a dummy schema for tests
            let schema =
                Schema::parse(include_str!("../tests/schemas/simple.graphql"), "").unwrap();
            let schema = schema.validate().unwrap();
            let schema = ValidFederationSchema::new(schema).unwrap();

            // Fill in some dummy data
            graph.add_node(federated_query_graph::Node::Concrete {
                supergraph_type: ObjectTypeDefinitionPosition {
                    type_name: name!("_query"),
                },
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::ConcreteNode::Connect(
                    connect::federated_query_graph::ConcreteNode::SelectionRoot {
                        subgraph_type: ObjectTypeDefinitionPosition {
                            type_name: name!("_query"),
                        },
                        property_path: Vec::new(),
                    },
                ),
            });

            SetupInfo {
                graph,
                schema,
                source_id,
            }
        }

        #[test]
        fn it_adds_operation_element_with_no_field_with_concrete() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let node_type = ObjectTypeDefinitionPosition {
                type_name: name!("_noFieldConcreteNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_noFieldTypeName"),
                field_name: name!("_noFieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Concrete {
                supergraph_type: node_type.clone(),
                field_edges: IndexMap::new(),
                source_exiting_edge: None,
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::ConcreteNode::Connect(
                    connect::federated_query_graph::ConcreteNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field("no_field_concrete".to_string())],
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: Default::default(),
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Connect {
                            subgraph_field: field_pos.clone(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: None,
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_noFieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_noFieldTestAlias",
                            arguments: {},
                            selections: Selections {
                                head_property_path: [
                                    Field(
                                        "no_field_concrete",
                                    ),
                                ],
                                named_selections: [],
                                tail_selection: None,
                            },
                        },
                    ),
                },
            )
            "###);
        }

        #[test]
        fn it_adds_operation_element_with_no_field_with_enum() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let node_type = EnumTypeDefinitionPosition {
                type_name: name!("_noFieldEnumNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_noFieldTypeName"),
                field_name: name!("_noFieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Enum {
                supergraph_type: node_type.clone(),
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::EnumNode::Connect(
                    connect::federated_query_graph::EnumNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field("no_field_enum".to_string())],
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: Default::default(),
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Connect {
                            subgraph_field: field_pos.clone(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: None,
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_noFieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_noFieldTestAlias",
                            arguments: {},
                            selections: Selections {
                                head_property_path: [
                                    Field(
                                        "no_field_enum",
                                    ),
                                ],
                                named_selections: [],
                                tail_selection: None,
                            },
                        },
                    ),
                },
            )
            "###);
        }

        #[test]
        fn it_adds_operation_element_with_no_field_with_custom_scalar() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let (_, selection) = JSONSelection::parse(".one.two.three").unwrap();
            let node_type = ScalarTypeDefinitionPosition {
                type_name: name!("_noFieldCustomScalarNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_noFieldTypeName"),
                field_name: name!("_noFieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Scalar {
                supergraph_type: node_type.clone(),
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::ScalarNode::Connect(
                    connect::federated_query_graph::ScalarNode::CustomScalarSelectionRoot {
                        subgraph_type: node_type.clone(),
                        selection,
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: Default::default(),
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Connect {
                            subgraph_field: field_pos.clone(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: None,
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_noFieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_noFieldTestAlias",
                            arguments: {},
                            selections: CustomScalarRoot {
                                selection: Path(
                                    Key(
                                        Field(
                                            "one",
                                        ),
                                        Key(
                                            Field(
                                                "two",
                                            ),
                                            Key(
                                                Field(
                                                    "three",
                                                ),
                                                Empty,
                                            ),
                                        ),
                                    ),
                                ),
                            },
                        },
                    ),
                },
            )
            "###);
        }

        #[test]
        fn it_adds_operation_element_with_existing_field_with_selection() {
            let SetupInfo {
                schema,
                source_id,
                mut graph,
                ..
            } = setup();

            let node_type = EnumTypeDefinitionPosition {
                type_name: name!("_fieldEnumNode"),
            };
            let field_pos = ObjectFieldDefinitionPosition {
                type_name: name!("_fieldTypeName"),
                field_name: name!("_fieldFieldName"),
            };
            let node = graph.add_node(federated_query_graph::Node::Enum {
                supergraph_type: node_type.clone(),
                source_id: source_id.clone(),
                source_data: source::federated_query_graph::EnumNode::Connect(
                    connect::federated_query_graph::EnumNode::SelectionRoot {
                        subgraph_type: node_type.clone(),
                        property_path: vec![Key::Field("field_enum".to_string())],
                    },
                ),
            });

            let edge = graph.add_edge(
                NodeIndex::new(0),
                node,
                federated_query_graph::Edge::ConcreteField {
                    supergraph_field: field_pos.clone(),
                    self_conditions: Default::default(),
                    source_id: source_id.clone(),
                    source_data: source::federated_query_graph::ConcreteFieldEdge::Connect(
                        connect::federated_query_graph::ConcreteFieldEdge::Selection {
                            subgraph_field: field_pos.clone(),
                            property_path: vec!["one", "two", "three"]
                                .into_iter()
                                .map(|prop| Key::Field(prop.to_string()))
                                .collect(),
                        },
                    ),
                },
            );
            let path = connect::fetch_dependency_graph::Path {
                merge_at: Arc::new([]),
                source_entering_edge: EdgeIndex::end(),
                source_id,
                field: Some(connect::fetch_dependency_graph::PathField {
                    response_name: name!("_connectPathResponseName"),
                    arguments: IndexMap::new(),
                    selections: connect::fetch_dependency_graph::PathSelections::Selections {
                        head_property_path: Vec::new(),
                        named_selections: Vec::new(),
                        tail_selection: None,
                    },
                }),
            };
            let operation_element = Arc::new(OperationPathElement::Field(Field::new(FieldData {
                schema,
                field_position: FieldDefinitionPosition::Object(field_pos),
                alias: Some(name!("_fieldTestAlias")),
                arguments: Arc::new(Vec::new()),
                directives: Arc::new(DirectiveList::new()),
                sibling_typename: None,
            })));

            let result = path
                .add_operation_element(
                    Arc::new(FederatedQueryGraph::with_graph(graph)),
                    operation_element,
                    Some(edge),
                    IndexMap::new(),
                )
                .unwrap();

            assert_debug_snapshot!(result, @r###"
            Connect(
                Path {
                    merge_at: [],
                    source_entering_edge: EdgeIndex(4294967295),
                    source_id: Connect(
                        ConnectId {
                            label: "test connect",
                            subgraph_name: "CONNECT",
                            directive: ObjectOrInterfaceFieldDirectivePosition {
                                field: Object(_testObject.testField),
                                directive_name: "connect",
                                directive_index: 0,
                            },
                        },
                    ),
                    field: Some(
                        PathField {
                            response_name: "_connectPathResponseName",
                            arguments: {},
                            selections: Selections {
                                head_property_path: [],
                                named_selections: [],
                                tail_selection: Some(
                                    (
                                        "_fieldTestAlias",
                                        Selection {
                                            property_path: [
                                                Field(
                                                    "one",
                                                ),
                                                Field(
                                                    "two",
                                                ),
                                                Field(
                                                    "three",
                                                ),
                                            ],
                                        },
                                    ),
                                ),
                            },
                        },
                    ),
                },
            )
            "###);
        }
    }
}
