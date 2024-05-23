use apollo_compiler::name;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::NamedType;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::is_graphql_reserved_name;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::Edge;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::Node;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::sources::source;
use crate::sources::source::SourceId;
use crate::sources::source::SourceKind;

pub(crate) fn build_federated_query_graph(
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    validate_extracted_subgraphs: Option<bool>,
    is_for_query_planning: Option<bool>,
) -> Result<FederatedQueryGraph, FederationError> {
    let is_for_query_planning = is_for_query_planning.unwrap_or(true);
    let subgraphs =
        extract_subgraphs_from_supergraph(&supergraph_schema, validate_extracted_subgraphs)?;

    let mut intra_source_builder =
        IntraSourceQueryGraphBuilder::new(supergraph_schema, api_schema, is_for_query_planning);
    let builders: source::federated_query_graph::builder::FederatedQueryGraphBuilders =
        Default::default();
    builders.process_subgraph_schemas(subgraphs, &mut intra_source_builder)?;
    let mut graph = intra_source_builder.graph;

    let supergraph_query_type_pos = ObjectTypeDefinitionPosition {
        type_name: name!("Query"),
    };
    if graph
        .supergraph_types_to_root_nodes
        .contains_key(&supergraph_query_type_pos)
    {
        graph
            .supergraph_root_kinds_to_types
            .insert(SchemaRootDefinitionKind::Query, supergraph_query_type_pos);
    }
    let supergraph_mutation_type_pos = ObjectTypeDefinitionPosition {
        type_name: name!("Mutation"),
    };
    if graph
        .supergraph_types_to_root_nodes
        .contains_key(&supergraph_mutation_type_pos)
    {
        graph.supergraph_root_kinds_to_types.insert(
            SchemaRootDefinitionKind::Mutation,
            supergraph_mutation_type_pos,
        );
    }
    let supergraph_subscription_type_pos = ObjectTypeDefinitionPosition {
        type_name: name!("Subscription"),
    };
    if graph
        .supergraph_types_to_root_nodes
        .contains_key(&supergraph_subscription_type_pos)
    {
        graph.supergraph_root_kinds_to_types.insert(
            SchemaRootDefinitionKind::Subscription,
            supergraph_subscription_type_pos,
        );
    }

    Ok(graph)
}

pub(crate) struct IntraSourceQueryGraphBuilder {
    graph: FederatedQueryGraph,
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    is_for_query_planning: bool,
    non_source_entering_supergraph_types_to_nodes:
        IndexMap<ObjectTypeDefinitionPosition, IndexSet<NodeIndex>>,
    supergraph_types_to_self_conditions:
        IndexMap<CompositeTypeDefinitionPosition, IndexSet<SelfConditionIndex>>,
    source_ids: IndexSet<SourceId>,
    source_kind: Option<SourceKind>,
}

impl IntraSourceQueryGraphBuilder {
    fn new(
        supergraph_schema: ValidFederationSchema,
        api_schema: ValidFederationSchema,
        is_for_query_planning: bool,
    ) -> Self {
        Self {
            // Note that we explicitly do not implement `Default` for `FederatedQueryGraph`, as that
            // data structure should only ever be constructed by this `new()` method.
            graph: FederatedQueryGraph {
                graph: Default::default(),
                supergraph_types_to_root_nodes: Default::default(),
                supergraph_root_kinds_to_types: Default::default(),
                self_conditions: Default::default(),
                non_trivial_followup_edges: Default::default(),
                source_data: Default::default(),
            },
            supergraph_schema,
            api_schema,
            is_for_query_planning,
            non_source_entering_supergraph_types_to_nodes: Default::default(),
            supergraph_types_to_self_conditions: Default::default(),
            source_ids: Default::default(),
            source_kind: None,
        }
    }

    pub(crate) fn set_source_kind(&mut self, source_kind: SourceKind) {
        self.source_kind = Some(source_kind);
    }
}

// This is declared as a trait to allow mocking in tests for source-specific code.
pub(crate) trait IntraSourceQueryGraphBuilderApi {
    fn source_query_graph(
        &mut self,
    ) -> Result<&mut source::federated_query_graph::FederatedQueryGraph, FederationError>;

    fn is_for_query_planning(&self) -> bool;

    fn add_source(
        &mut self,
        source: SourceId,
    ) -> Result<impl IntraSourceQueryGraphSubBuilderApi, FederationError>;
}

impl IntraSourceQueryGraphBuilderApi for IntraSourceQueryGraphBuilder {
    fn source_query_graph(
        &mut self,
    ) -> Result<&mut source::federated_query_graph::FederatedQueryGraph, FederationError> {
        let Some(source_kind) = self.source_kind else {
            return Err(FederationError::internal(
                "Source kind unexpectedly absent.",
            ));
        };
        self.graph
            .source_data
            .graphs
            .get_mut(&source_kind)
            .ok_or_else(|| FederationError::internal("Source data unexpectedly missing."))
    }

    fn is_for_query_planning(&self) -> bool {
        self.is_for_query_planning
    }

    fn add_source(
        &mut self,
        source: SourceId,
    ) -> Result<impl IntraSourceQueryGraphSubBuilderApi, FederationError> {
        if Some(source.kind()) != self.source_kind {
            return Err(FederationError::internal(format!(
                r#"Source "{source}" did not match kind "{}"."#,
                source.kind(),
            )));
        }
        if !self.source_ids.insert(source.clone()) {
            return Err(FederationError::internal(format!(
                r#"Source "{source}" has already been added."#
            )));
        }
        Ok(IntraSourceQueryGraphSubBuilder {
            source_id: source,
            builder: self,
        })
    }
}

struct IntraSourceQueryGraphSubBuilder<'builder> {
    source_id: SourceId,
    builder: &'builder mut IntraSourceQueryGraphBuilder,
}

// This is declared as a trait to allow mocking in tests for source-specific code.
pub(crate) trait IntraSourceQueryGraphSubBuilderApi {
    fn source_query_graph(
        &mut self,
    ) -> Result<&mut source::federated_query_graph::FederatedQueryGraph, FederationError>;

    fn is_for_query_planning(&self) -> bool;

    fn get_source(&self) -> SourceId;

    fn add_self_condition(
        &mut self,
        supergraph_type_name: NamedType,
        field_set: &str,
    ) -> Result<Option<SelfConditionIndex>, FederationError>;

    fn add_abstract_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::AbstractNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_concrete_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::ConcreteNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_enum_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::EnumNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_scalar_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::ScalarNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_abstract_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        source_data: source::federated_query_graph::AbstractFieldEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_concrete_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: source::federated_query_graph::ConcreteFieldEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_type_condition_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        source_data: source::federated_query_graph::TypeConditionEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_source_entering_edge(
        &mut self,
        tail: NodeIndex,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: source::federated_query_graph::SourceEnteringEdge,
    ) -> Result<EdgeIndex, FederationError>;
}

impl<'builder> IntraSourceQueryGraphSubBuilderApi for IntraSourceQueryGraphSubBuilder<'builder> {
    fn source_query_graph(
        &mut self,
    ) -> Result<&mut source::federated_query_graph::FederatedQueryGraph, FederationError> {
        self.builder.source_query_graph()
    }

    fn is_for_query_planning(&self) -> bool {
        self.builder.is_for_query_planning()
    }

    fn get_source(&self) -> SourceId {
        self.source_id.clone()
    }

    fn add_self_condition(
        &mut self,
        supergraph_type_name: NamedType,
        field_set: &str,
    ) -> Result<Option<SelfConditionIndex>, FederationError> {
        let supergraph_type_pos: CompositeTypeDefinitionPosition = self
            .builder
            .supergraph_schema
            .get_type(supergraph_type_name)?
            .try_into()?;
        let self_condition_selection_set = parse_field_set(
            &self.builder.supergraph_schema,
            supergraph_type_pos.type_name().clone(),
            field_set,
        )?;
        if self_condition_selection_set.selections.is_empty() {
            return Ok(None);
        }
        for other_self_condition in self
            .builder
            .supergraph_types_to_self_conditions
            .get(&supergraph_type_pos)
            .iter()
            .copied()
            .flatten()
        {
            let other_self_condition_selection_set = self
                .builder
                .graph
                .self_conditions
                .get(other_self_condition.0)
                .ok_or_else(|| {
                    FederationError::internal("Unexpectedly invalid self condition index")
                })?;
            if self_condition_selection_set == *other_self_condition_selection_set {
                return Ok(Some(*other_self_condition));
            }
        }
        let self_condition = SelfConditionIndex(self.builder.graph.self_conditions.len());
        self.builder
            .graph
            .self_conditions
            .push(self_condition_selection_set);
        self.builder
            .supergraph_types_to_self_conditions
            .entry(supergraph_type_pos)
            .or_default()
            .insert(self_condition);
        Ok(Some(self_condition))
    }

    fn add_abstract_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::AbstractNode,
    ) -> Result<NodeIndex, FederationError> {
        let supergraph_type_pos: AbstractTypeDefinitionPosition = self
            .builder
            .supergraph_schema
            .get_type(supergraph_type_name)?
            .try_into()?;
        Ok(self.builder.graph.graph.add_node(Node::Abstract {
            supergraph_type: supergraph_type_pos,
            field_edges: Default::default(),
            concrete_type_condition_edges: Default::default(),
            abstract_type_condition_edges: Default::default(),
            source_id: self.source_id.clone(),
            source_data,
        }))
    }

    fn add_concrete_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::ConcreteNode,
    ) -> Result<NodeIndex, FederationError> {
        let supergraph_type_pos: ObjectTypeDefinitionPosition = self
            .builder
            .supergraph_schema
            .get_type(supergraph_type_name)?
            .try_into()?;
        let node = self.builder.graph.graph.add_node(Node::Concrete {
            supergraph_type: supergraph_type_pos.clone(),
            field_edges: Default::default(),
            source_exiting_edge: None,
            source_id: self.source_id.clone(),
            source_data,
        });
        if let Some(root) = self
            .builder
            .graph
            .supergraph_types_to_root_nodes
            .get(&supergraph_type_pos)
        {
            let edge = self.builder.graph.graph.add_edge(
                node,
                *root,
                Edge::SourceExiting {
                    supergraph_type: supergraph_type_pos,
                    head_source_id: self.source_id.clone(),
                },
            );
            let Node::Concrete {
                source_exiting_edge,
                ..
            } = self.builder.graph.node_weight_mut(node)?
            else {
                return Err(FederationError::internal(
                    "Cannot add source-exiting edge for non-concrete head node.",
                ));
            };
            *source_exiting_edge = Some(edge);
        } else {
            self.builder
                .non_source_entering_supergraph_types_to_nodes
                .entry(supergraph_type_pos)
                .or_default()
                .insert(node);
        }
        Ok(node)
    }

    fn add_enum_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::EnumNode,
    ) -> Result<NodeIndex, FederationError> {
        let supergraph_type_pos: EnumTypeDefinitionPosition = self
            .builder
            .supergraph_schema
            .get_type(supergraph_type_name)?
            .try_into()?;
        Ok(self.builder.graph.graph.add_node(Node::Enum {
            supergraph_type: supergraph_type_pos,
            source_id: self.source_id.clone(),
            source_data,
        }))
    }

    fn add_scalar_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::ScalarNode,
    ) -> Result<NodeIndex, FederationError> {
        let supergraph_type_pos: ScalarTypeDefinitionPosition = self
            .builder
            .supergraph_schema
            .get_type(supergraph_type_name)?
            .try_into()?;
        Ok(self.builder.graph.graph.add_node(Node::Scalar {
            supergraph_type: supergraph_type_pos,
            source_id: self.source_id.clone(),
            source_data,
        }))
    }

    fn add_abstract_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        source_data: source::federated_query_graph::AbstractFieldEdge,
    ) -> Result<EdgeIndex, FederationError> {
        if is_graphql_reserved_name(&supergraph_field_name) {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for GraphQL reserved name.",
            ));
        }

        let Node::Abstract {
            supergraph_type: head_supergraph_type_pos,
            field_edges,
            source_id: head_source_id,
            ..
        } = self.builder.graph.node_weight(head)?
        else {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for non-abstract head node.",
            ));
        };
        if *head_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for head node from a different source.",
            ));
        }
        let supergraph_field_pos = head_supergraph_type_pos.field(supergraph_field_name)?;
        if field_edges.contains_key(&supergraph_field_pos) {
            return Err(FederationError::internal(
                "Cannot add abstract field edge since a field edge with the same name exists.",
            ));
        }

        let tail_weight = self.builder.graph.node_weight(tail)?;
        let Some(tail_source_id) = tail_weight.source_id() else {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for tail node without a source.",
            ));
        };
        if *tail_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for tail node from a different source.",
            ));
        }
        let supergraph_field_type_name = supergraph_field_pos
            .get(self.builder.supergraph_schema.schema())?
            .ty
            .inner_named_type();
        if supergraph_field_type_name != tail_weight.supergraph_type().type_name() {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for tail node of the wrong type.",
            ));
        }

        let edge = self.builder.graph.graph.add_edge(
            head,
            tail,
            Edge::AbstractField {
                supergraph_field: supergraph_field_pos.clone(),
                matches_concrete_options: false,
                source_id: self.source_id.clone(),
                source_data,
            },
        );
        let Node::Abstract { field_edges, .. } = self.builder.graph.node_weight_mut(head)? else {
            return Err(FederationError::internal(
                "Cannot add abstract field edge for non-abstract head node.",
            ));
        };
        field_edges.insert(supergraph_field_pos, edge);
        Ok(edge)
    }

    fn add_concrete_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: source::federated_query_graph::ConcreteFieldEdge,
    ) -> Result<EdgeIndex, FederationError> {
        if is_graphql_reserved_name(&supergraph_field_name) {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for GraphQL reserved name.",
            ));
        }

        let Node::Concrete {
            supergraph_type: head_supergraph_type_pos,
            field_edges,
            source_id: head_source_id,
            ..
        } = self.builder.graph.node_weight(head)?
        else {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for non-concrete head node.",
            ));
        };
        if *head_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for head node from a different source.",
            ));
        }
        let supergraph_field_pos = head_supergraph_type_pos.field(supergraph_field_name);
        if field_edges.contains_key(&supergraph_field_pos) {
            return Err(FederationError::internal(
                "Cannot add concrete field edge since a field edge with the same name exists.",
            ));
        }

        let tail_weight = self.builder.graph.node_weight(tail)?;
        let Some(tail_source_id) = tail_weight.source_id() else {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for tail node without a source.",
            ));
        };
        if *tail_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for tail node from a different source.",
            ));
        }
        let supergraph_field_type_name = supergraph_field_pos
            .get(self.builder.supergraph_schema.schema())?
            .ty
            .inner_named_type();
        if supergraph_field_type_name != tail_weight.supergraph_type().type_name() {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for tail node of the wrong type.",
            ));
        }

        let self_supergraph_type_pos: CompositeTypeDefinitionPosition =
            head_supergraph_type_pos.clone().into();
        if !self
            .builder
            .supergraph_types_to_self_conditions
            .get(&self_supergraph_type_pos)
            .map(|known_self_conditions| known_self_conditions.is_superset(&self_conditions))
            .unwrap_or_else(|| self_conditions.is_empty())
        {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for self conditions not registered on head type.",
            ));
        }

        let edge = self.builder.graph.graph.add_edge(
            head,
            tail,
            Edge::ConcreteField {
                supergraph_field: supergraph_field_pos.clone(),
                self_conditions,
                source_id: self.source_id.clone(),
                source_data,
            },
        );
        let Node::Concrete { field_edges, .. } = self.builder.graph.node_weight_mut(head)? else {
            return Err(FederationError::internal(
                "Cannot add concrete field edge for non-concrete head node.",
            ));
        };
        field_edges.insert(supergraph_field_pos, edge);
        Ok(edge)
    }

    fn add_type_condition_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        source_data: source::federated_query_graph::TypeConditionEdge,
    ) -> Result<EdgeIndex, FederationError> {
        let Node::Abstract {
            supergraph_type: head_supergraph_type_pos,
            concrete_type_condition_edges,
            abstract_type_condition_edges,
            source_id: head_source_id,
            ..
        } = self.builder.graph.node_weight(head)?
        else {
            return Err(FederationError::internal(
                "Cannot add type condition edge for non-abstract head node.",
            ));
        };
        if *head_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add type condition edge for head node from a different source.",
            ));
        }

        let tail_weight = self.builder.graph.node_weight(tail)?;
        let Some(tail_source_id) = tail_weight.source_id() else {
            return Err(FederationError::internal(
                "Cannot add type condition edge for tail node without a source.",
            ));
        };
        if *tail_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add type condition edge for tail node from a different source.",
            ));
        }
        let tail_supergraph_type_pos: CompositeTypeDefinitionPosition =
            tail_weight.supergraph_type().try_into()?;
        if self
            .builder
            .supergraph_schema
            .possible_runtime_types(head_supergraph_type_pos.clone().into())?
            .is_disjoint(
                &self
                    .builder
                    .supergraph_schema
                    .possible_runtime_types(tail_supergraph_type_pos.clone())?,
            )
        {
            return Err(FederationError::internal(
                "Cannot add type condition edge between types with disjoint runtime types.",
            ));
        }
        let is_tail_abstract = matches!(tail_weight, Node::Abstract { .. });
        let is_exists = if is_tail_abstract {
            abstract_type_condition_edges.contains_key(
                &TryInto::<AbstractTypeDefinitionPosition>::try_into(
                    tail_supergraph_type_pos.clone(),
                )?,
            )
        } else {
            concrete_type_condition_edges.contains_key(
                &TryInto::<ObjectTypeDefinitionPosition>::try_into(
                    tail_supergraph_type_pos.clone(),
                )?,
            )
        };
        if is_exists {
            return Err(FederationError::internal(
                "Cannot add type condition edge since a type condition edge with the same tail type exists.",
            ));
        }

        let edge = self.builder.graph.graph.add_edge(
            head,
            tail,
            Edge::TypeCondition {
                supergraph_type: tail_supergraph_type_pos.clone(),
                source_id: self.source_id.clone(),
                source_data,
            },
        );
        let Node::Abstract {
            concrete_type_condition_edges,
            abstract_type_condition_edges,
            ..
        } = self.builder.graph.node_weight_mut(head)?
        else {
            return Err(FederationError::internal(
                "Cannot add type condition edge for non-abstract head node.",
            ));
        };
        if is_tail_abstract {
            abstract_type_condition_edges.insert(
                TryInto::<AbstractTypeDefinitionPosition>::try_into(tail_supergraph_type_pos)?,
                edge,
            );
        } else {
            concrete_type_condition_edges.insert(
                TryInto::<ObjectTypeDefinitionPosition>::try_into(tail_supergraph_type_pos)?,
                edge,
            );
        };
        Ok(edge)
    }

    fn add_source_entering_edge(
        &mut self,
        tail: NodeIndex,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: source::federated_query_graph::SourceEnteringEdge,
    ) -> Result<EdgeIndex, FederationError> {
        let Node::Concrete {
            supergraph_type: tail_supergraph_type_pos,
            source_id: tail_source_id,
            ..
        } = self.builder.graph.node_weight(tail)?
        else {
            return Err(FederationError::internal(
                "Cannot add source-entering edge for non-concrete tail node.",
            ));
        };
        if *tail_source_id != self.source_id {
            return Err(FederationError::internal(
                "Cannot add source-entering edge for tail node from a different source.",
            ));
        }

        let self_supergraph_type_pos: CompositeTypeDefinitionPosition =
            tail_supergraph_type_pos.clone().into();
        if !self
            .builder
            .supergraph_types_to_self_conditions
            .get(&self_supergraph_type_pos)
            .map(|known_self_conditions| known_self_conditions.is_superset(&self_conditions))
            .unwrap_or_else(|| self_conditions.is_empty())
        {
            return Err(FederationError::internal(
                "Cannot add source-entering edge for self conditions not registered on tail type.",
            ));
        }

        let tail_supergraph_type_pos = tail_supergraph_type_pos.clone();
        let root = if let Some(root) = self
            .builder
            .graph
            .supergraph_types_to_root_nodes
            .get(&tail_supergraph_type_pos)
        {
            *root
        } else {
            let root = self.builder.graph.graph.add_node(Node::Root {
                supergraph_type: tail_supergraph_type_pos.clone(),
                source_entering_edges: Default::default(),
            });
            self.builder
                .graph
                .supergraph_types_to_root_nodes
                .insert(tail_supergraph_type_pos.clone(), root);
            for head in self
                .builder
                .non_source_entering_supergraph_types_to_nodes
                .swap_remove(&tail_supergraph_type_pos)
                .iter()
                .flatten()
            {
                let edge = self.builder.graph.graph.add_edge(
                    *head,
                    root,
                    Edge::SourceExiting {
                        supergraph_type: tail_supergraph_type_pos.clone(),
                        head_source_id: self.source_id.clone(),
                    },
                );
                let Node::Concrete {
                    source_exiting_edge,
                    ..
                } = self.builder.graph.node_weight_mut(*head)?
                else {
                    return Err(FederationError::internal(
                        "Cannot add source-exiting edge for non-concrete head node.",
                    ));
                };
                *source_exiting_edge = Some(edge);
            }
            root
        };

        let edge = self.builder.graph.graph.add_edge(
            root,
            tail,
            Edge::SourceEntering {
                supergraph_type: tail_supergraph_type_pos.clone(),
                self_conditions,
                tail_source_id: self.source_id.clone(),
                source_data,
            },
        );
        let Node::Root {
            source_entering_edges,
            ..
        } = self.builder.graph.node_weight_mut(root)?
        else {
            return Err(FederationError::internal(
                "Cannot add source-entering edge for non-root head node.",
            ));
        };
        source_entering_edges
            .entry(tail)
            .or_insert_with(Default::default)
            .insert(edge);
        Ok(edge)
    }
}
