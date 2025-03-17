use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::validation::Valid;
use itertools::Itertools;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use regex::Regex;

use super::base_query_graph::BaseQueryGraphBuilder;
use super::base_query_graph::SchemaQueryGraphBuilder;
use super::base_query_graph::resolvable_key_applications;
use crate::bail;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::operation::merge_selection_sets;
use crate::query_graph::ContextCondition;
use crate::query_graph::OverrideCondition;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::supergraph::extract_subgraphs_from_supergraph;
use crate::utils::FallibleIterator;
use crate::utils::iter_into_single_item;

/// Builds a "federated" query graph based on the provided supergraph and API schema.
///
/// A federated query graph is one that is used to reason about queries made by a router against a
/// set of federated subgraph services.
///
/// Assumes the given schemas have been validated.
pub fn build_federated_query_graph(
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    // TODO(@goto-bus-stop): replace these booleans by descriptive types (either a struct or two
    // enums)
    validate_extracted_subgraphs: Option<bool>,
    for_query_planning: Option<bool>,
) -> Result<QueryGraph, FederationError> {
    let for_query_planning = for_query_planning.unwrap_or(true);
    let query_graph = QueryGraph {
        // Note this name is a dummy initial name that gets overridden as we build the query graph.
        current_source: "".into(),
        graph: Default::default(),
        sources: Default::default(),
        subgraphs_by_name: Default::default(),
        supergraph_schema: Default::default(),
        types_to_nodes_by_source: Default::default(),
        root_kinds_to_nodes_by_source: Default::default(),
        non_trivial_followup_edges: Default::default(),
        arguments_to_context_ids_by_source: Default::default(),
    };
    let query_graph =
        extract_subgraphs_from_supergraph(&supergraph_schema, validate_extracted_subgraphs)?
            .into_iter()
            .fallible_fold(query_graph, |query_graph, (subgraph_name, subgraph)| {
                SchemaQueryGraphBuilder::new(
                    query_graph,
                    subgraph_name,
                    subgraph.schema,
                    Some(api_schema.clone()),
                    for_query_planning,
                )?
                .build()
            })?;
    FederatedQueryGraphBuilder::new(query_graph, supergraph_schema)?.build()
}

/// Builds a query graph based on the provided schema (usually an API schema outside of testing).
///
/// Assumes the given schemas have been validated.
pub fn build_query_graph(
    name: Arc<str>,
    schema: ValidFederationSchema,
) -> Result<QueryGraph, FederationError> {
    let mut query_graph = QueryGraph {
        // Note this name is a dummy initial name that gets overridden as we build the query graph.
        current_source: "".into(),
        graph: Default::default(),
        sources: Default::default(),
        subgraphs_by_name: Default::default(),
        supergraph_schema: Default::default(),
        types_to_nodes_by_source: Default::default(),
        root_kinds_to_nodes_by_source: Default::default(),
        non_trivial_followup_edges: Default::default(),
        arguments_to_context_ids_by_source: Default::default(),
    };
    let builder = SchemaQueryGraphBuilder::new(query_graph, name, schema, None, false)?;
    query_graph = builder.build()?;
    Ok(query_graph)
}

struct FederatedQueryGraphBuilder {
    base: BaseQueryGraphBuilder,
    supergraph_schema: ValidFederationSchema,
    subgraphs: FederatedQueryGraphBuilderSubgraphs,
}

impl FederatedQueryGraphBuilder {
    fn new(
        mut query_graph: QueryGraph,
        supergraph_schema: ValidFederationSchema,
    ) -> Result<Self, FederationError> {
        query_graph.supergraph_schema = Some(supergraph_schema.clone());
        let base = BaseQueryGraphBuilder::new(
            query_graph,
            FEDERATED_GRAPH_ROOT_SOURCE.into(),
            // This is a dummy schema that should never be used, so it's fine if we assume validity
            // here (note that empty schemas have no Query type, making them invalid GraphQL).
            ValidFederationSchema::new(Valid::assume_valid(Schema::new()))?,
        );

        let subgraphs = FederatedQueryGraphBuilderSubgraphs::new(&base)?;
        Ok(FederatedQueryGraphBuilder {
            base,
            supergraph_schema,
            subgraphs,
        })
    }

    fn build(mut self) -> Result<QueryGraph, FederationError> {
        self.copy_subgraphs();
        self.add_federated_root_nodes()?;
        self.copy_types_to_nodes()?;
        self.add_root_edges()?;
        self.handle_key()?;
        self.handle_requires()?;
        self.handle_progressive_overrides()?;
        self.handle_context()?;
        // Note that @provides must be handled last when building since it requires copying nodes
        // and their edges, and it's easier to reason about this if we know previous
        self.handle_provides()?;
        // The exception to the above rule is @interaceObject handling, where we explicitly don't
        // want to add self-edges for copied @provides nodes. (See the comments in this method for
        // more details).
        self.handle_interface_object()?;
        // This method adds no nodes/edges, but just precomputes followup edge information.
        self.precompute_non_trivial_followup_edges()?;
        Ok(self.base.build())
    }

    fn copy_subgraphs(&mut self) {
        for (source, schema) in &self.base.query_graph.sources {
            if *source == self.base.query_graph.current_source {
                continue;
            }
            self.base
                .query_graph
                .subgraphs_by_name
                .insert(source.clone(), schema.clone());
        }
    }

    fn add_federated_root_nodes(&mut self) -> Result<(), FederationError> {
        let root_kinds = self
            .base
            .query_graph
            .root_kinds_to_nodes_by_source
            .iter()
            .filter(|(source, _)| **source != self.base.query_graph.current_source)
            .flat_map(|(_, root_kind_to_nodes)| root_kind_to_nodes.keys().copied())
            .collect::<IndexSet<_>>();
        for root_kind in root_kinds {
            self.base.create_root_node(root_kind.into(), root_kind)?;
        }
        Ok(())
    }

    fn copy_types_to_nodes(&mut self) -> Result<(), FederationError> {
        let mut federated_type_to_nodes = IndexMap::default();
        for (source, types_to_nodes) in &self.base.query_graph.types_to_nodes_by_source {
            if *source == self.base.query_graph.current_source {
                continue;
            }
            for (type_name, nodes) in types_to_nodes {
                let federated_nodes: &mut IndexSet<_> = federated_type_to_nodes
                    .entry(type_name.clone())
                    .or_default();
                for node in nodes {
                    federated_nodes.insert(*node);
                }
            }
        }
        *self.base.query_graph.types_to_nodes_mut()? = federated_type_to_nodes;
        Ok(())
    }

    /// Add the edges from supergraph roots to the subgraph ones. Also, for each root kind, we also
    /// add edges from the corresponding root type of each subgraph to the root type of other
    /// subgraphs (and for @defer, like for @key, we also add self-node loops). This encodes the
    /// fact that if a field returns a root type, we can always query any subgraph from that point.
    fn add_root_edges(&mut self) -> Result<(), FederationError> {
        let mut new_edges = Vec::new();
        for (source, root_kinds_to_nodes) in &self.base.query_graph.root_kinds_to_nodes_by_source {
            if *source == self.base.query_graph.current_source {
                continue;
            }
            for (root_kind, root_node) in root_kinds_to_nodes {
                let federated_root_node = self
                    .base
                    .query_graph
                    .root_kinds_to_nodes()?
                    .get(root_kind)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: "Federated root node unexpectedly missing".to_owned(),
                    })?;
                new_edges.push(QueryGraphEdgeData {
                    head: *federated_root_node,
                    tail: *root_node,
                    transition: QueryGraphEdgeTransition::SubgraphEnteringTransition,
                    conditions: None,
                });
                for (other_source, other_root_kinds_to_nodes) in
                    &self.base.query_graph.root_kinds_to_nodes_by_source
                {
                    if *other_source == self.base.query_graph.current_source {
                        continue;
                    }
                    if let Some(other_root_node) = other_root_kinds_to_nodes.get(root_kind) {
                        new_edges.push(QueryGraphEdgeData {
                            head: *root_node,
                            tail: *other_root_node,
                            transition: QueryGraphEdgeTransition::RootTypeResolution {
                                root_kind: *root_kind,
                            },
                            conditions: None,
                        })
                    }
                }
            }
        }
        for new_edge in new_edges {
            new_edge.add_to(&mut self.base)?;
        }
        Ok(())
    }

    /// Handle @key by adding the appropriate key-resolution edges.
    fn handle_key(&mut self) -> Result<(), FederationError> {
        // We'll look at adding edges from "other subgraphs" to the current type. So the tail of
        // all the edges we'll build here is always going to be the same.
        for tail in self.base.query_graph.graph.node_indices() {
            let mut new_edges = Vec::new();
            let tail_weight = self.base.query_graph.node_weight(tail)?;
            let source = &tail_weight.source;
            if *source == self.base.query_graph.current_source {
                continue;
            }
            // Ignore federated root nodes.
            let QueryGraphNodeType::SchemaType(type_pos) = &tail_weight.type_ else {
                continue;
            };
            let schema = self.base.query_graph.schema_by_source(source)?;
            let directives = schema
                .schema()
                .types
                .get(type_pos.type_name())
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!(
                        "Type \"{}\" unexpectedly missing from subgraph \"{}\"",
                        type_pos, source,
                    ),
                })?
                .directives();
            let subgraph_data = self.subgraphs.get(source)?;
            let is_interface_object = matches!(type_pos, OutputTypeDefinitionPosition::Object(_))
                && directives.has(&subgraph_data.interface_object_directive_definition_name);

            for application in resolvable_key_applications(
                directives,
                &subgraph_data.key_directive_definition_name,
                subgraph_data.federation_spec_definition,
            )? {
                // The @key directive creates an edge from every subgraph having that type to
                // the current subgraph. In other words, the fact this subgraph has a @key means
                // that the current subgraph can be queried for the entity (through _entities)
                // as long as "the other side" can provide the proper field values. Note that we
                // only require that "the other side" can gather the key fields (through
                // the path conditions; note that it's possible those conditions are never
                // satisfiable), but we don't care that it defines the same key, because it's
                // not a technical requirement (and in general while we probably don't want to
                // allow a type to be an entity in some subgraphs but not others, this is not
                // the place to impose that restriction, and this may be at least temporarily
                // useful to allow convert a type to an entity).
                let Ok(type_pos): Result<ObjectOrInterfaceTypeDefinitionPosition, _> =
                    type_pos.clone().try_into()
                else {
                    return Err(SingleFederationError::Internal {
                            message: format!(
                                "Invalid \"@key\" application on non-object/interface type \"{}\" in subgraph \"{}\"",
                                type_pos,
                                source,
                            )
                        }.into());
                };
                let conditions = Arc::new(parse_field_set(
                    schema,
                    type_pos.type_name().clone(),
                    application.fields,
                )?);

                // Note that each subgraph has a key edge to itself (when head == tail below).
                // We usually ignore these edges, but they exist for the special case of @defer,
                // where we technically may have to take such "edges to self" as meaning to
                // "re-enter" a subgraph for a deferred section.
                for (other_source, other_types_to_nodes) in
                    &self.base.query_graph.types_to_nodes_by_source
                {
                    if *other_source == self.base.query_graph.current_source {
                        continue;
                    }

                    let other_nodes = other_types_to_nodes.get(type_pos.type_name());
                    if let Some(other_nodes) = other_nodes {
                        let head = other_nodes.first().ok_or_else(|| {
                            SingleFederationError::Internal {
                                message: format!(
                                    "Types-to-nodes set unexpectedly empty for type \"{}\" in subgraph \"{}\"",
                                    type_pos,
                                    other_source,
                                ),
                            }
                        })?;
                        // Note that later, when we've handled @provides, this might not be true
                        // anymore as @provides may create copy of a certain type. But for now, it's
                        // true.
                        if other_nodes.len() > 1 {
                            return Err(
                                SingleFederationError::Internal {
                                    message: format!(
                                        "Types-to-nodes set unexpectedly had more than one element for type \"{}\" in subgraph \"{}\"",
                                        type_pos,
                                        other_source,
                                    ),
                                }
                                    .into()
                            );
                        }
                        // The edge goes from the other subgraph to this one.
                        new_edges.push(QueryGraphEdgeData {
                            head: *head,
                            tail,
                            transition: QueryGraphEdgeTransition::KeyResolution,
                            conditions: Some(conditions.clone()),
                        })
                    }

                    // Additionally, if the key is on an @interfaceObject and this "other" subgraph
                    // has some of the implementations of the corresponding interface, then we need
                    // an edge from each of those implementations (to the @interfaceObject). This is
                    // used when an entity of a specific implementation is queried first, but then
                    // some of the requested fields are only provided by that @interfaceObject.
                    if is_interface_object {
                        let type_in_supergraph_pos = self
                            .supergraph_schema
                            .get_type(type_pos.type_name().clone())?;
                        let TypeDefinitionPosition::Interface(type_in_supergraph_pos) =
                            type_in_supergraph_pos
                        else {
                            return Err(SingleFederationError::Internal {
                                    message: format!(
                                        "Type \"{}\" was marked with \"@interfaceObject\" in subgraph \"{}\", but was non-interface in supergraph",
                                        type_pos,
                                        other_source,
                                    )
                                }.into());
                        };
                        for implementation_type_in_supergraph_pos in self
                            .supergraph_schema
                            .possible_runtime_types(type_in_supergraph_pos.into())?
                        {
                            // That implementation type may or may not exists in the "other
                            // subgraph". If it doesn't, we just have nothing to do for that
                            // particular implementation. If it does, we'll add the proper edge, but
                            // note that we're guaranteed to have at most one node for the same
                            // reasons as mentioned above (only the handling of @provides will make
                            // it so that there can be more than one node per type).
                            let Some(implementation_nodes) = self
                                .base
                                .query_graph
                                .types_to_nodes_by_source(other_source)?
                                .get(&implementation_type_in_supergraph_pos.type_name)
                            else {
                                continue;
                            };
                            let head = implementation_nodes.first().ok_or_else(|| {
                                SingleFederationError::Internal {
                                    message: format!(
                                        "Types-to-nodes set unexpectedly empty for type \"{}\" in subgraph \"{}\"",
                                        implementation_type_in_supergraph_pos,
                                        other_source,
                                    ),
                                }
                            })?;
                            if implementation_nodes.len() > 1 {
                                return Err(
                                    SingleFederationError::Internal {
                                        message: format!(
                                            "Types-to-nodes set unexpectedly had more than one element for type \"{}\" in subgraph \"{}\"",
                                            implementation_type_in_supergraph_pos,
                                            other_source,
                                        ),
                                    }.into()
                                );
                            }

                            // The key goes from the implementation type in the "other subgraph" to
                            // the @interfaceObject type in this one, so the `conditions` will be
                            // "fetched" on the implementation type, but the `conditions` have been
                            // parsed on the @interfaceObject type. We'd like it to use fields from
                            // the implementation type and not the @interfaceObject type, so we
                            // re-parse the condition using the implementation type. This could
                            // fail, but in that case it just means that key is not usable in the
                            // other subgraph.
                            let other_schema =
                                self.base.query_graph.schema_by_source(other_source)?;
                            let implementation_type_in_other_subgraph_pos: CompositeTypeDefinitionPosition =
                                other_schema.get_type(implementation_type_in_supergraph_pos.type_name.clone())?.try_into()?;
                            let Ok(implementation_conditions) = parse_field_set(
                                other_schema,
                                implementation_type_in_other_subgraph_pos
                                    .type_name()
                                    .clone(),
                                application.fields,
                            ) else {
                                // Ignored on purpose: it just means the key is not usable on this
                                // subgraph.
                                continue;
                            };
                            new_edges.push(QueryGraphEdgeData {
                                head: *head,
                                tail,
                                transition: QueryGraphEdgeTransition::KeyResolution,
                                conditions: Some(Arc::new(implementation_conditions)),
                            })
                        }
                    }
                }
            }
            for new_edge in new_edges {
                new_edge.add_to(&mut self.base)?;
            }
        }
        Ok(())
    }

    /// Handle @requires by updating the appropriate field-collecting edges.
    fn handle_requires(&mut self) -> Result<(), FederationError> {
        // We'll look at any field-collecting edges with @requires and adding their conditions to
        // those edges.
        for edge in self.base.query_graph.graph.edge_indices() {
            let edge_weight = self.base.query_graph.edge_weight(edge)?;
            let QueryGraphEdgeTransition::FieldCollection {
                source,
                field_definition_position,
                ..
            } = &edge_weight.transition
            else {
                continue;
            };
            if *source == self.base.query_graph.current_source {
                continue;
            }
            // Nothing prior to this should have set any conditions for field-collecting edges.
            // This won't be the case after this method though.
            if edge_weight.conditions.is_some() {
                return Err(SingleFederationError::Internal {
                    message: format!(
                        "Field-collection edge for field \"{}\" unexpectedly had conditions",
                        field_definition_position,
                    ),
                }
                .into());
            }
            let schema = self.base.query_graph.schema_by_source(source)?;
            let subgraph_data = self.subgraphs.get(source)?;
            let field = field_definition_position.get(schema.schema())?;
            let mut all_conditions = Vec::new();
            for directive in field
                .directives
                .get_all(&subgraph_data.requires_directive_definition_name)
            {
                let application = subgraph_data
                    .federation_spec_definition
                    .requires_directive_arguments(directive)?;
                // @requires field set is validated against the supergraph
                let conditions = parse_field_set(
                    &self.supergraph_schema,
                    field_definition_position.parent().type_name().clone(),
                    application.fields,
                )?;
                all_conditions.push(conditions);
            }
            if all_conditions.is_empty() {
                continue;
            }
            // PORT_NOTE: I'm not sure when this list will ever not be a singleton list, but it's
            // like this way in the JS codebase, so we'll mimic the behavior for now.
            //
            // TODO: This is an optimization to avoid unnecessary inter-conversion between
            // the apollo-rs operation representation and the apollo-federation one. This wasn't a
            // problem in the JS codebase, as it would use its own operation representation from
            // the start. Eventually when operation processing code is ready and we make the switch
            // to using the apollo-federation representation everywhere, we can probably simplify
            // this.
            let new_conditions = if all_conditions.len() == 1 {
                all_conditions
                    .pop()
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: "Singleton list was unexpectedly empty".to_owned(),
                    })?
            } else {
                merge_selection_sets(all_conditions)?
            };
            let edge_weight_mut = self.base.query_graph.edge_weight_mut(edge)?;
            edge_weight_mut.conditions = Some(Arc::new(new_conditions));
        }
        Ok(())
    }

    /// Handling progressive overrides here. For each progressive @override
    /// application (with a label), we want to update the edges to the overridden
    /// field within the "to" and "from" subgraphs with their respective override
    /// condition (the label and a T/F value). The "from" subgraph will have an
    /// override condition of `false`, whereas the "to" subgraph will have an
    /// override condition of `true`.
    fn handle_progressive_overrides(&mut self) -> Result<(), FederationError> {
        let mut edge_to_conditions: IndexMap<EdgeIndex, OverrideCondition> = Default::default();

        fn collect_edge_condition(
            query_graph: &QueryGraph,
            target_graph: &str,
            target_field: &ObjectFieldDefinitionPosition,
            label: &str,
            condition: bool,
            edge_to_conditions: &mut IndexMap<EdgeIndex, OverrideCondition>,
        ) -> Result<(), FederationError> {
            let target_field = FieldDefinitionPosition::Object(target_field.clone());
            let subgraph_nodes = query_graph
                .types_to_nodes_by_source
                .get(target_graph)
                .unwrap();
            let parent_node = subgraph_nodes
                .get(target_field.type_name())
                .unwrap()
                .first()
                .unwrap();
            for edge in query_graph.out_edges(*parent_node) {
                let edge_weight = query_graph.edge_weight(edge.id())?;
                let QueryGraphEdgeTransition::FieldCollection {
                    field_definition_position,
                    ..
                } = &edge_weight.transition
                else {
                    continue;
                };

                if &target_field == field_definition_position {
                    edge_to_conditions.insert(
                        edge.id(),
                        OverrideCondition {
                            label: label.to_string(),
                            condition,
                        },
                    );
                }
            }
            Ok(())
        }

        for (to_subgraph_name, subgraph) in &self.base.query_graph.subgraphs_by_name {
            let subgraph_data = self.subgraphs.get(to_subgraph_name)?;
            if let Some(override_referencers) = subgraph
                .referencers()
                .directives
                .get(&subgraph_data.overrides_directive_definition_name)
            {
                for field_definition_position in &override_referencers.object_fields {
                    let field = field_definition_position.get(subgraph.schema())?;
                    for directive in field
                        .directives
                        .get_all(&subgraph_data.overrides_directive_definition_name)
                    {
                        let application = subgraph_data
                            .federation_spec_definition
                            .override_directive_arguments(directive)?;
                        if let Some(label) = application.label {
                            collect_edge_condition(
                                &self.base.query_graph,
                                to_subgraph_name,
                                field_definition_position,
                                label,
                                true,
                                &mut edge_to_conditions,
                            )?;
                            collect_edge_condition(
                                &self.base.query_graph,
                                application.from,
                                field_definition_position,
                                label,
                                false,
                                &mut edge_to_conditions,
                            )?;
                        }
                    }
                }
            }
        }

        for (edge, condition) in edge_to_conditions {
            let mutable_edge = self.base.query_graph.edge_weight_mut(edge)?;
            mutable_edge.override_condition = Some(condition);
        }
        Ok(())
    }

    fn handle_context(&mut self) -> Result<(), FederationError> {
        let mut subgraph_to_args: IndexMap<Arc<str>, Vec<ObjectFieldArgumentDefinitionPosition>> =
            Default::default();
        let mut coordinate_map: IndexMap<
            Arc<str>,
            IndexMap<ObjectFieldDefinitionPosition, Vec<ContextCondition>>,
        > = Default::default();
        for (subgraph_name, subgraph) in self.base.query_graph.subgraphs() {
            let subgraph_data = self.subgraphs.get(subgraph_name)?;
            let Some(context_refs) = &subgraph
                .referencers()
                .directives
                .get(&subgraph_data.context_directive_definition_name)
            else {
                continue;
            };
            let Some(from_context_refs) = &subgraph
                .referencers()
                .directives
                .get(&subgraph_data.from_context_directive_definition_name)
            else {
                continue;
            };

            // Collect data for @context
            let mut context_name_to_types: IndexMap<
                &str,
                IndexSet<CompositeTypeDefinitionPosition>,
            > = Default::default();
            for object_def_pos in &context_refs.object_types {
                let object = object_def_pos.get(subgraph.schema())?;
                for dir in object
                    .directives
                    .get_all(subgraph_data.context_directive_definition_name.as_str())
                {
                    let application = subgraph_data
                        .federation_spec_definition
                        .context_directive_arguments(dir)?;
                    context_name_to_types
                        .entry(application.name)
                        .or_default()
                        .insert(object_def_pos.clone().into());
                }
            }
            for interface_def_pos in &context_refs.interface_types {
                let interface = interface_def_pos.get(subgraph.schema())?;
                for dir in interface
                    .directives
                    .get_all(subgraph_data.context_directive_definition_name.as_str())
                {
                    let application = subgraph_data
                        .federation_spec_definition
                        .context_directive_arguments(dir)?;
                    context_name_to_types
                        .entry(application.name)
                        .or_default()
                        .insert(interface_def_pos.clone().into());
                }
            }
            for union_def_pos in &context_refs.union_types {
                let union = union_def_pos.get(subgraph.schema())?;
                for dir in union
                    .directives
                    .get_all(subgraph_data.context_directive_definition_name.as_str())
                {
                    let application = subgraph_data
                        .federation_spec_definition
                        .context_directive_arguments(dir)?;
                    context_name_to_types
                        .entry(application.name)
                        .or_default()
                        .insert(union_def_pos.clone().into());
                }
            }

            // Collect data for @fromContext
            let coordinate_map = coordinate_map.entry(subgraph_name.clone()).or_default();
            for object_field_arg in &from_context_refs.object_field_arguments {
                let input_value = object_field_arg.get(subgraph.schema())?;
                subgraph_to_args
                    .entry(subgraph_name.clone())
                    .or_default()
                    .push(object_field_arg.clone());
                let field_coordinate = object_field_arg.parent();
                let Some(dir) = input_value.directives.get(
                    subgraph_data
                        .from_context_directive_definition_name
                        .as_str(),
                ) else {
                    bail!(
                        "Argument {} unexpectedly missing @fromContext directive",
                        object_field_arg
                    );
                };
                let application = subgraph_data
                    .federation_spec_definition
                    .from_context_directive_arguments(dir)?;
                let (context, selection) = parse_context(application.field)?;
                let Some(types_with_context_set) = context_name_to_types.get(context.as_str())
                else {
                    bail!(
                        r#"Context ${} is never set in subgraph "{}""#,
                        context,
                        subgraph_name
                    );
                };
                let conditions = ContextCondition {
                    context,
                    subgraph_name: subgraph_name.clone(),
                    selection,
                    types_with_context_set: types_with_context_set.clone(),
                    argument_name: object_field_arg.argument_name.to_owned(),
                    argument_coordinate: object_field_arg.clone(),
                    argument_type: input_value.ty.clone(),
                };
                coordinate_map
                    .entry(field_coordinate.clone())
                    .or_default()
                    .push(conditions);
            }
        }

        for edge in self.base.query_graph.graph.edge_indices() {
            let edge_weight = self.base.query_graph.edge_weight(edge)?;
            let QueryGraphEdgeTransition::FieldCollection {
                source,
                field_definition_position,
                ..
            } = &edge_weight.transition
            else {
                continue;
            };
            let FieldDefinitionPosition::Object(obj_field) = field_definition_position else {
                continue;
            };
            let Some(contexts) = coordinate_map.get_mut(source) else {
                continue;
            };
            let Some(required_contexts) = contexts.get(obj_field) else {
                continue;
            };
            self.base
                .query_graph
                .edge_weight_mut(edge)?
                .required_contexts
                .extend_from_slice(required_contexts);
        }

        // Add the context argument mapping
        self.base.query_graph.arguments_to_context_ids_by_source = self
            .base
            .query_graph
            .subgraphs()
            .enumerate()
            .filter_map(|(index, (source, _))| {
                subgraph_to_args
                    .get_key_value(source)
                    .map(|(source, args)| (index, source, args))
            })
            .map(|(index, source, args)| {
                Ok::<_, FederationError>((
                    source.clone(),
                    args.iter()
                        // TODO: We're manually sorting by the actual GraphQL coordinate string here
                        //       to mimic the behavior of JS code. In the future, we could just sort
                        //       the argument position in the natural tuple-based way.
                        .sorted_by_key(|arg| {
                            format!(
                                "{}.{}({}:)",
                                arg.type_name, arg.field_name, arg.argument_name
                            )
                        })
                        .enumerate()
                        .map(|(i, arg)| {
                            Ok::<_, FederationError>((
                                arg.clone(),
                                format!("contextualArgument_{}_{}", index + 1, i).try_into()?,
                            ))
                        })
                        .process_results(|r| r.collect())?,
                ))
            })
            .process_results(|r| r.collect())?;

        Ok(())
    }

    /// Handle @provides by copying the appropriate nodes/edges.
    fn handle_provides(&mut self) -> Result<(), FederationError> {
        let mut provide_id = 0;
        for edge in self.base.query_graph.graph.edge_indices() {
            let edge_weight = self.base.query_graph.edge_weight(edge)?;
            let QueryGraphEdgeTransition::FieldCollection {
                source,
                field_definition_position,
                ..
            } = &edge_weight.transition
            else {
                continue;
            };
            if *source == self.base.query_graph.current_source {
                continue;
            }
            let source = source.clone();
            let schema = self.base.query_graph.schema_by_source(&source)?;
            let subgraph_data = self.subgraphs.get(&source)?;
            let field = field_definition_position.get(schema.schema())?;
            let field_type_pos = schema.get_type(field.ty.inner_named_type().clone())?;
            let mut all_conditions = Vec::new();
            for directive in field
                .directives
                .get_all(&subgraph_data.provides_directive_definition_name)
            {
                let application = subgraph_data
                    .federation_spec_definition
                    .provides_directive_arguments(directive)?;
                let field_type_pos: CompositeTypeDefinitionPosition =
                    field_type_pos.clone().try_into()?;
                let conditions = parse_field_set(
                    schema,
                    field_type_pos.type_name().clone(),
                    application.fields,
                )?;
                all_conditions.push(conditions);
            }
            if all_conditions.is_empty() {
                continue;
            }
            provide_id += 1;
            // PORT_NOTE: I'm not sure when this list will ever not be a singleton list, but it's
            // like this way in the JS codebase, so we'll mimic the behavior for now.
            //
            // Note that the JS codebase had a bug here when there was more than one selection set,
            // where it would copy per @provides application, but only point the field-collecting
            // edge toward the last copy. We do something similar to @requires instead, where we
            // merge the selection sets before into one.
            //
            // TODO: This is an optimization to avoid unnecessary inter-conversion between
            // the apollo-rs operation representation and the apollo-federation one. This wasn't a
            // problem in the JS codebase, as it would use its own operation representation from
            // the start. Eventually when operation processing code is ready and we make the switch
            // to using the apollo-federation representation everywhere, we can probably simplify
            // this.
            let new_conditions = if all_conditions.len() == 1 {
                all_conditions
                    .pop()
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: "Singleton list was unexpectedly empty".to_owned(),
                    })?
            } else {
                merge_selection_sets(all_conditions)?
            };
            // We make a copy of the tail node (representing the field's type) with all the same
            // out-edges, and we change this particular in-edge to point to the new copy. We then
            // add all the provides edges starting from the copy.
            let (_, tail) = self.base.query_graph.edge_endpoints(edge)?;
            let new_tail = Self::copy_for_provides(&mut self.base, tail, provide_id)?;
            Self::update_edge_tail(&mut self.base, edge, new_tail)?;
            Self::add_provides_edges(
                &mut self.base,
                &source,
                new_tail,
                &new_conditions,
                provide_id,
            )?;
        }
        Ok(())
    }

    fn add_provides_edges(
        base: &mut BaseQueryGraphBuilder,
        source: &Arc<str>,
        head: NodeIndex,
        provided: &SelectionSet,
        provide_id: u32,
    ) -> Result<(), FederationError> {
        let mut stack = vec![(head, provided)];
        while let Some((node, selection_set)) = stack.pop() {
            // We reverse-iterate through the selections to cancel out the reversing that the stack
            // does.
            for selection in selection_set.selections.values().rev() {
                match selection {
                    Selection::Field(field_selection) => {
                        let existing_edge_info = base
                            .query_graph
                            .out_edges_with_federation_self_edges(node)
                            .into_iter()
                            .find_map(|edge_ref| {
                                let edge_weight = edge_ref.weight();
                                let QueryGraphEdgeTransition::FieldCollection {
                                    field_definition_position,
                                    ..
                                } = &edge_weight.transition
                                else {
                                    return None;
                                };
                                if field_definition_position.field_name()
                                    == field_selection.field.name()
                                {
                                    Some((edge_ref.id(), edge_ref.target()))
                                } else {
                                    None
                                }
                            });
                        if let Some((edge, tail)) = existing_edge_info {
                            // If this is a leaf field, then we don't really have anything to do.
                            // Otherwise, we need to copy the tail and continue propagating the
                            // provided selections from there.
                            if let Some(selections) = &field_selection.selection_set {
                                let new_tail = Self::copy_for_provides(base, tail, provide_id)?;
                                Self::update_edge_tail(base, edge, new_tail)?;
                                stack.push((new_tail, selections))
                            }
                        } else {
                            // There are no existing edges, which means that it's an edge added by
                            // an @provides to an @external field. We find the existing node it
                            // leads to.
                            //
                            // PORT_NOTE: The JS codebase would create the node if it didn't exist,
                            // but this is a holdover from when subgraph query graph building
                            // would skip @external fields completely instead of just recursing
                            // into the type.
                            //
                            // Also, the JS codebase appears to have a bug where they find some node
                            // with the appropriate source and type name, but they don't guarantee
                            // that the node has no provide_id (i.e. it may be a copied node), which
                            // means they may have extra edges that accordingly can't be taken. We
                            // fix this below by filtering by provide_id.
                            let field = field_selection
                                .field
                                .field_position
                                .get(field_selection.field.schema.schema())?;
                            let tail_type = field.ty.inner_named_type();
                            let possible_tails = base
                                .query_graph
                                .types_to_nodes_by_source(source)?
                                .get(tail_type)
                                .ok_or_else(|| SingleFederationError::Internal {
                                    message: format!(
                                        "Types-to-nodes map missing type \"{}\" in subgraph \"{}\"",
                                        tail_type, source,
                                    ),
                                })?;
                            // Note because this is an IndexSet, the non-provides should be first
                            // since it was added first, but it's a bit fragile so we fallback to
                            // checking the whole set if it's not first.
                            let mut tail: Option<NodeIndex> = None;
                            for possible_tail in possible_tails {
                                let possible_tail_weight =
                                    base.query_graph.node_weight(*possible_tail)?;
                                if possible_tail_weight.provide_id.is_none() {
                                    tail = Some(*possible_tail);
                                    break;
                                }
                            }
                            let Some(tail) = tail else {
                                return Err(
                                    SingleFederationError::Internal {
                                        message: format!(
                                            "Missing non-provides node for type \"{}\" in subgraph \"{}\"",
                                            tail_type,
                                            source,
                                        )
                                    }.into()
                                );
                            };
                            // If this is a leaf field, then just create the new edge and we're
                            // done. Otherwise, we should copy the node, add the edge, and continue
                            // propagating.
                            let transition = QueryGraphEdgeTransition::FieldCollection {
                                source: source.clone(),
                                field_definition_position: field_selection
                                    .field
                                    .field_position
                                    .clone(),
                                is_part_of_provides: true,
                            };
                            if let Some(selections) = &field_selection.selection_set {
                                let new_tail = Self::copy_for_provides(base, tail, provide_id)?;
                                base.add_edge(node, new_tail, transition, None)?;
                                stack.push((new_tail, selections))
                            } else {
                                base.add_edge(node, tail, transition, None)?;
                            }
                        }
                    }
                    Selection::InlineFragment(inline_fragment_selection) => {
                        if let Some(type_condition_pos) = &inline_fragment_selection
                            .inline_fragment
                            .type_condition_position
                        {
                            // We should always have an edge: otherwise it would mean we list a type
                            // condition for a type that isn't in the subgraph, but the @provides
                            // shouldn't have validated in the first place (another way to put this
                            // is, contrary to fields, there is no way currently to mark a full type
                            // as @external).
                            //
                            // TODO: Query graph creation during composition only creates Downcast
                            // edges between abstract types and their possible runtime object types,
                            // so the requirement above effectively prohibits the use of type
                            // conditions containing abstract types in @provides. This is fine for
                            // query planning since we have the same Downcast edges (additionally
                            // for query planning, there are usually edges between abstract types
                            // when their intersection is at least size 2). However, when this code
                            // gets used in composition, the assertion below will be the error users
                            // will see in those cases, and it would be better to do a formal check
                            // with better messaging during merging instead of during query graph
                            // construction.
                            let (edge, tail) = base
                                .query_graph
                                .out_edges_with_federation_self_edges(node)
                                .into_iter()
                                .find_map(|edge_ref| {
                                    let edge_weight = edge_ref.weight();
                                    let QueryGraphEdgeTransition::Downcast {
                                        to_type_position, ..
                                    } = &edge_weight.transition
                                    else {
                                        return None;
                                    };
                                    if to_type_position == type_condition_pos {
                                        Some((edge_ref.id(), edge_ref.target()))
                                    } else {
                                        None
                                    }
                                })
                                .ok_or_else(|| {
                                    SingleFederationError::Internal {
                                        message: format!(
                                            "Shouldn't have selection \"{}\" in an @provides, as its type condition has no query graph edge",
                                            inline_fragment_selection,
                                        )
                                    }
                                })?;
                            let new_tail = Self::copy_for_provides(base, tail, provide_id)?;
                            Self::update_edge_tail(base, edge, new_tail)?;
                            stack.push((new_tail, &inline_fragment_selection.selection_set))
                        } else {
                            // Essentially ignore the condition in this case, and continue
                            // propagating the provided selections.
                            stack.push((node, &inline_fragment_selection.selection_set));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn update_edge_tail(
        base: &mut BaseQueryGraphBuilder,
        edge: EdgeIndex,
        tail: NodeIndex,
    ) -> Result<(), FederationError> {
        // Note that petgraph has no method to directly update an edge's endpoints. Instead, you
        // must:
        // 1. Add a new edge, with the same weight but updated endpoints. This edge then becomes the
        //    last edge in the graph (i.e. the one with highest edge index).
        // 2. Remove the old edge. As per the API docs, this causes the last edge's index to change
        //    to be the one that was removed.
        // This results in a Graph where it looks like only the edge's endpoints have changed while
        // its index and weight are unchanged, but really its weight has been cloned (which is
        // cheap).
        let (new_edge_head, _) = base.query_graph.edge_endpoints(edge)?;
        let new_edge_weight = base.query_graph.edge_weight(edge)?.clone();
        base.query_graph
            .graph
            .add_edge(new_edge_head, tail, new_edge_weight);
        base.query_graph.graph.remove_edge(edge);
        Ok(())
    }

    /// Copies the given node and its outgoing edges, returning the new node's index.
    fn copy_for_provides(
        base: &mut BaseQueryGraphBuilder,
        node: NodeIndex,
        provide_id: u32,
    ) -> Result<NodeIndex, FederationError> {
        let current_source = base.query_graph.current_source.clone();
        let result = Self::copy_for_provides_internal(base, node, provide_id);
        // Ensure that the current source resets, even if the copy unexpectedly fails.
        base.query_graph.current_source = current_source;
        let (new_node, type_pos) = result?;
        base.query_graph
            .types_to_nodes_mut()?
            .get_mut(type_pos.type_name())
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!(
                    "Unexpectedly missing @provides type \"{}\" in types-to-nodes map",
                    type_pos,
                ),
            })?
            .insert(new_node);
        Ok(new_node)
    }

    fn copy_for_provides_internal(
        base: &mut BaseQueryGraphBuilder,
        node: NodeIndex,
        provide_id: u32,
    ) -> Result<(NodeIndex, OutputTypeDefinitionPosition), FederationError> {
        let node_weight = base.query_graph.node_weight(node)?;
        let QueryGraphNodeType::SchemaType(type_pos) = node_weight.type_.clone() else {
            return Err(SingleFederationError::Internal {
                message: "Unexpectedly found @provides for federated root node".to_owned(),
            }
            .into());
        };
        let has_reachable_cross_subgraph_edges = node_weight.has_reachable_cross_subgraph_edges;
        base.query_graph.current_source = node_weight.source.clone();
        let new_node = base.create_new_node(type_pos.clone().into())?;
        let new_node_weight = base.query_graph.node_weight_mut(new_node)?;
        new_node_weight.provide_id = Some(provide_id);
        new_node_weight.has_reachable_cross_subgraph_edges = has_reachable_cross_subgraph_edges;

        let mut new_edges = Vec::new();
        for edge_ref in base.query_graph.out_edges_with_federation_self_edges(node) {
            let edge_tail = edge_ref.target();
            let edge_weight = edge_ref.weight();
            new_edges.push(QueryGraphEdgeData {
                head: new_node,
                tail: edge_tail,
                transition: edge_weight.transition.clone(),
                conditions: edge_weight.conditions.clone(),
            });
        }
        for new_edge in new_edges {
            new_edge.add_to(base)?;
        }
        Ok((new_node, type_pos))
    }

    /// Handle @interfaceObject by adding the appropriate fake-downcast self-edges.
    fn handle_interface_object(&mut self) -> Result<(), FederationError> {
        // There are cases where only an/some implementation(s) of an interface are queried, and
        // that could apply to an interface that is an @interfaceObject in some subgraph. Consider
        // the following example:
        // ```graphql
        // type Query {
        //   getIs: [I]
        // }
        //
        // type I @key(fields: "id") @interfaceObject {
        //   id: ID!
        //   x: Int
        // }
        // ```
        // where we suppose that `I` has some implementations `A`, `B`, and `C` in some other subgraph.
        // Now, consider query:
        // ```graphql
        // {
        //   getIs {
        //     ... on B {
        //       x
        //     }
        //   }
        // }
        // ```
        // So here, we query `x` (which the subgraph provides) but we only do so for one of the
        // implementations. So in that case, we essentially need to figure out the `__typename` first
        // (or more precisely, we need to know the real __typename "eventually"; we could theoretically
        // query `x` first, and then get the __typename to know if we should keep the result or discard
        // it, and that could be more efficient in certain cases, but as we don't know both 1) if `x`
        // is expansive to resolve and 2) what ratio of the results from `getIs` will be `B` versus some
        // other implementation, it is "safer" to get the __typename first and only resolve `x` when we
        // need to).
        //
        // Long story short, to solve this, we create edges from @interfaceObject types to themselves
        // for every implementation type of the interface: those edges will be taken when we try to take
        // a `... on B` condition, and those edges have __typename have a condition, forcing us to find
        // __typename in another subgraph first.
        let mut new_edges = Vec::new();
        for (source, schema) in &self.base.query_graph.sources {
            if *source == self.base.query_graph.current_source {
                continue;
            }
            let subgraph_data = self.subgraphs.get(source)?;
            for type_pos in &schema
                .referencers()
                .get_directive(&subgraph_data.interface_object_directive_definition_name)?
                .object_types
            {
                // This method is run after handling @provides, so there may be multiple nodes here
                // instead of just one. We specifically just want to add the self-edges to the
                // non-provides node. This is because any @provides in a subgraph whose field set
                // contains an @interfaceObject object type couldn't actually place a type condition
                // on that object type, since that field set must validate against the subgraph
                // schema (and object types have no intersection with other object types, even if
                // the subgraph had any implementation types). This is also why we run this method
                // after the @provides handling instead of before (since we don't want to clone
                // the self-edges added by this method).
                let possible_nodes = self
                    .base
                    .query_graph
                    .types_to_nodes_by_source(source)?
                    .get(&type_pos.type_name)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Types-to-nodes map missing type \"{}\" in subgraph \"{}\"",
                            type_pos, source,
                        ),
                    })?;
                // Note because this is an IndexSet, the non-provides should be first since it was
                // added first, but it's a bit fragile so we fallback to checking the whole set if
                // it's not first.
                let mut node: Option<NodeIndex> = None;
                for possible_node in possible_nodes {
                    let possible_tail_weight = self.base.query_graph.node_weight(*possible_node)?;
                    if possible_tail_weight.provide_id.is_none() {
                        node = Some(*possible_node);
                        break;
                    }
                }
                let Some(node) = node else {
                    return Err(SingleFederationError::Internal {
                        message: format!(
                            "Missing non-provides node for type \"{}\" in subgraph \"{}\"",
                            type_pos, source,
                        ),
                    }
                    .into());
                };
                let type_in_supergraph_pos = self
                    .supergraph_schema
                    .get_type(type_pos.type_name.clone())?;
                let TypeDefinitionPosition::Interface(type_in_supergraph_pos) =
                    type_in_supergraph_pos
                else {
                    return Err(SingleFederationError::Internal {
                            message: format!(
                                "Type \"{}\" was marked with \"@interfaceObject\" in subgraph \"{}\", but was non-interface in supergraph",
                                type_pos,
                                source,
                            )
                        }.into());
                };
                let conditions = Arc::new(parse_field_set(
                    schema,
                    type_in_supergraph_pos.type_name.clone(),
                    "__typename",
                )?);
                for implementation_type_in_supergraph_pos in self
                    .supergraph_schema
                    .possible_runtime_types(type_in_supergraph_pos.into())?
                {
                    let transition = QueryGraphEdgeTransition::InterfaceObjectFakeDownCast {
                        source: source.clone(),
                        from_type_position: type_pos.clone().into(),
                        to_type_name: implementation_type_in_supergraph_pos.type_name,
                    };
                    new_edges.push(QueryGraphEdgeData {
                        head: node,
                        tail: node,
                        transition,
                        conditions: Some(conditions.clone()),
                    });
                }
            }
        }
        for new_edge in new_edges {
            new_edge.add_to(&mut self.base)?;
        }
        Ok(())
    }

    /// Precompute which followup edges for a given edge are non-trivial.
    fn precompute_non_trivial_followup_edges(&mut self) -> Result<(), FederationError> {
        for edge in self.base.query_graph.graph.edge_indices() {
            let edge_weight = self.base.query_graph.edge_weight(edge)?;
            let (_, tail) = self.base.query_graph.edge_endpoints(edge)?;
            let out_edges = self.base.query_graph.out_edges(tail);
            let mut non_trivial_followups = Vec::with_capacity(out_edges.len());
            for followup_edge_ref in out_edges {
                let followup_edge_weight = followup_edge_ref.weight();
                match edge_weight.transition {
                    QueryGraphEdgeTransition::KeyResolution => {
                        // After taking a key from subgraph A to B, there is no point of following
                        // that up with another key to subgraph C if that key has the same
                        // conditions. This is because, due to the way key edges are created, if we
                        // have a key (with some conditions X) from B to C, then we are guaranteed
                        // to also have a key (with the same conditions X) from A to C, and so it's
                        // that later key we should be using in the first place. In other words,
                        // it's never better to do 2 hops rather than 1.
                        if matches!(
                            followup_edge_weight.transition,
                            QueryGraphEdgeTransition::KeyResolution
                        ) {
                            let Some(conditions) = &edge_weight.conditions else {
                                return Err(SingleFederationError::Internal {
                                    message: "Key resolution edge unexpectedly missing conditions"
                                        .to_owned(),
                                }
                                .into());
                            };
                            let Some(followup_conditions) = &followup_edge_weight.conditions else {
                                return Err(SingleFederationError::Internal {
                                    message: "Key resolution edge unexpectedly missing conditions"
                                        .to_owned(),
                                }
                                .into());
                            };

                            if conditions == followup_conditions {
                                continue;
                            }
                        }
                    }
                    QueryGraphEdgeTransition::RootTypeResolution { .. } => {
                        // A 'RootTypeResolution' means that a query reached the query type (or
                        // another root type) in some subgraph A and we're looking at jumping to
                        // another subgraph B. But like for keys, there is no point in trying to
                        // jump directly to yet another subpraph C from B, since we can always jump
                        // directly from A to C and it's better.
                        if matches!(
                            followup_edge_weight.transition,
                            QueryGraphEdgeTransition::RootTypeResolution { .. }
                        ) {
                            continue;
                        }
                    }
                    QueryGraphEdgeTransition::SubgraphEnteringTransition => {
                        // This is somewhat similar to 'RootTypeResolution' except that we're
                        // starting the query. Still, we shouldn't do "start of query" -> B -> C,
                        // since we can do "start of query" -> C and that's always better.
                        if matches!(
                            followup_edge_weight.transition,
                            QueryGraphEdgeTransition::RootTypeResolution { .. }
                        ) {
                            continue;
                        }
                    }
                    _ => {}
                }
                non_trivial_followups.push(followup_edge_ref.id());
            }
            self.base
                .query_graph
                .non_trivial_followup_edges
                .insert(edge, non_trivial_followups);
        }
        Ok(())
    }
}

const FEDERATED_GRAPH_ROOT_SOURCE: &str = "_";

struct FederatedQueryGraphBuilderSubgraphs {
    map: IndexMap<Arc<str>, FederatedQueryGraphBuilderSubgraphData>,
}

impl FederatedQueryGraphBuilderSubgraphs {
    fn new(base: &BaseQueryGraphBuilder) -> Result<Self, FederationError> {
        let mut subgraphs = FederatedQueryGraphBuilderSubgraphs {
            map: IndexMap::default(),
        };
        for (source, schema) in &base.query_graph.sources {
            if *source == base.query_graph.current_source {
                continue;
            }
            let federation_spec_definition = get_federation_spec_definition_from_subgraph(schema)?;
            let key_directive_definition_name = federation_spec_definition
                .key_directive_definition(schema)?
                .name
                .clone();
            let requires_directive_definition_name = federation_spec_definition
                .requires_directive_definition(schema)?
                .name
                .clone();
            let provides_directive_definition_name = federation_spec_definition
                .provides_directive_definition(schema)?
                .name
                .clone();
            let interface_object_directive_definition_name = federation_spec_definition
                .interface_object_directive_definition(schema)?
                .map(|d| d.name.clone())
                .ok_or_else(|| {
                    // Extracted subgraphs should use the latest federation spec version, which
                    // means they should include an @interfaceObject definition.
                    SingleFederationError::Internal {
                        message: format!(
                            "Subgraph \"{}\" unexpectedly missing @interfaceObject definition",
                            source,
                        ),
                    }
                })?;
            let overrides_directive_definition_name = federation_spec_definition
                .override_directive_definition(schema)?
                .name
                .clone();
            let context_directive_definition_name = federation_spec_definition
                .context_directive_definition(schema)?
                .name
                .clone();
            let from_context_directive_definition_name = federation_spec_definition
                .from_context_directive_definition(schema)?
                .name
                .clone();
            subgraphs.map.insert(
                source.clone(),
                FederatedQueryGraphBuilderSubgraphData {
                    federation_spec_definition,
                    key_directive_definition_name,
                    requires_directive_definition_name,
                    provides_directive_definition_name,
                    interface_object_directive_definition_name,
                    overrides_directive_definition_name,
                    context_directive_definition_name,
                    from_context_directive_definition_name,
                },
            );
        }
        Ok(subgraphs)
    }

    fn get(
        &self,
        source: &str,
    ) -> Result<&FederatedQueryGraphBuilderSubgraphData, FederationError> {
        self.map.get(source).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Subgraph data unexpectedly missing".to_owned(),
            }
            .into()
        })
    }
}

struct FederatedQueryGraphBuilderSubgraphData {
    federation_spec_definition: &'static FederationSpecDefinition,
    key_directive_definition_name: Name,
    requires_directive_definition_name: Name,
    provides_directive_definition_name: Name,
    interface_object_directive_definition_name: Name,
    overrides_directive_definition_name: Name,
    context_directive_definition_name: Name,
    from_context_directive_definition_name: Name,
}

#[derive(Debug)]
struct QueryGraphEdgeData {
    head: NodeIndex,
    tail: NodeIndex,
    transition: QueryGraphEdgeTransition,
    conditions: Option<Arc<SelectionSet>>,
}

impl QueryGraphEdgeData {
    fn add_to(self, builder: &mut BaseQueryGraphBuilder) -> Result<(), FederationError> {
        builder.add_edge(self.head, self.tail, self.transition, self.conditions)
    }
}

static CONTEXT_PARSING_LEADING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:[\n\r\t ,]|#[^\n\r]*)*((?s:.)*)$"#).unwrap());

static CONTEXT_PARSING_CONTEXT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Za-z_](?-u:\w)*)((?s:.)*)$"#).unwrap());

fn context_parse_error(context: &str, message: &str) -> FederationError {
    SingleFederationError::FromContextParseError {
        context: context.to_string(),
        message: message.to_string(),
    }
    .into()
}

pub(crate) fn parse_context(field: &str) -> Result<(String, String), FederationError> {
    // PORT_NOTE: The original JS regex, as shown below
    //   /^(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*\$(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*([A-Za-z_]\w*(?!\w))([\s\S]*)$/
    // makes use of negative lookaheads, which aren't supported natively by Rust's regex crate.
    // There's a fancy_regex crate which does support this, but in the specific case above, the
    // negative lookaheads are just used to ensure strict *-greediness for the preceding expression
    // (i.e., it guarantees those *-expressions match greedily and won't backtrack).
    //
    // We can emulate that in this case by matching a series of regexes instead of a single regex,
    // where for each regex, the relevant *-expression doesn't backtrack by virtue of the rest of
    // the haystack guaranteeing a match. Also note that Rust has (?s:.) to match all characters
    // including newlines, which we use in place of JS's common regex workaround of [\s\S].
    fn strip_leading_ignored_tokens(input: &str) -> Result<&str, FederationError> {
        iter_into_single_item(CONTEXT_PARSING_LEADING_PATTERN.captures_iter(input))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .ok_or_else(|| context_parse_error(input, "Failed to skip any leading ignored tokens"))
    }

    let dollar_start = strip_leading_ignored_tokens(field)?;
    let mut dollar_iter = dollar_start.chars();
    if dollar_iter.next() != Some('$') {
        return Err(context_parse_error(
            dollar_start,
            r#"Failed to find leading "$""#,
        ));
    }
    let after_dollar = dollar_iter.as_str();

    let context_start = strip_leading_ignored_tokens(after_dollar)?;
    let Some(context_captures) =
        iter_into_single_item(CONTEXT_PARSING_CONTEXT_PATTERN.captures_iter(context_start))
    else {
        return Err(context_parse_error(
            dollar_start,
            "Failed to find context name token and selection",
        ));
    };

    let context = match context_captures.get(1).map(|m| m.as_str()) {
        Some(context) if !context.is_empty() => context,
        _ => {
            return Err(context_parse_error(
                context_start,
                "Expected to find non-empty context name",
            ));
        }
    };

    let selection = match context_captures.get(2).map(|m| m.as_str()) {
        Some(selection) if !selection.is_empty() => selection,
        _ => {
            return Err(context_parse_error(
                context_start,
                "Expected to find non-empty selection",
            ));
        }
    };

    // PORT_NOTE: apollo_compiler's parsing code for field sets requires ignored tokens to be
    // pre-stripped if curly braces are missing, so we additionally do that here.
    let selection = strip_leading_ignored_tokens(selection)?;
    Ok((context.to_owned(), selection.to_owned()))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Name;
    use apollo_compiler::Schema;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::collections::IndexSet;
    use apollo_compiler::name;
    use petgraph::Direction;
    use petgraph::graph::NodeIndex;
    use petgraph::visit::EdgeRef;

    use crate::error::FederationError;
    use crate::query_graph::QueryGraph;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::query_graph::QueryGraphNode;
    use crate::query_graph::QueryGraphNodeType;
    use crate::query_graph::build_query_graph::build_query_graph;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::position::OutputTypeDefinitionPosition;
    use crate::schema::position::ScalarTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;

    const SCHEMA_NAME: &str = "test";

    fn test_query_graph_from_schema_sdl(sdl: &str) -> Result<QueryGraph, FederationError> {
        let schema =
            ValidFederationSchema::new(Schema::parse_and_validate(sdl, "schema.graphql")?)?;
        build_query_graph(SCHEMA_NAME.into(), schema)
    }

    fn assert_node_type(
        query_graph: &QueryGraph,
        node: NodeIndex,
        output_type_definition_position: OutputTypeDefinitionPosition,
        root_kind: Option<SchemaRootDefinitionKind>,
    ) -> Result<(), FederationError> {
        assert_eq!(
            *query_graph.node_weight(node)?,
            QueryGraphNode {
                type_: QueryGraphNodeType::SchemaType(output_type_definition_position),
                source: SCHEMA_NAME.into(),
                has_reachable_cross_subgraph_edges: false,
                provide_id: None,
                root_kind,
            },
        );
        Ok(())
    }

    fn named_edges(
        query_graph: &QueryGraph,
        head: NodeIndex,
        field_names: IndexSet<Name>,
    ) -> Result<IndexMap<Name, NodeIndex>, FederationError> {
        let mut result = IndexMap::default();
        for field_name in field_names {
            // PORT_NOTE: In the JS codebase, there were a lot of asserts here, but they were all
            // duplicated with single_edge() (or they tested the JS codebase's graph representation,
            // which we don't need to do since we're using petgraph), so they're all removed here.
            let tail = single_edge(query_graph, head, field_name.clone())?;
            result.insert(field_name, tail);
        }
        Ok(result)
    }

    fn single_edge(
        query_graph: &QueryGraph,
        head: NodeIndex,
        field_name: Name,
    ) -> Result<NodeIndex, FederationError> {
        let head_weight = query_graph.node_weight(head)?;
        let QueryGraphNodeType::SchemaType(type_pos) = &head_weight.type_ else {
            panic!("Unexpectedly found federated root type");
        };
        let type_pos: ObjectOrInterfaceTypeDefinitionPosition = type_pos.clone().try_into()?;
        let field_pos = type_pos.field(field_name);
        let schema = query_graph.schema()?;
        field_pos.get(schema.schema())?;
        let expected_field_transition = QueryGraphEdgeTransition::FieldCollection {
            source: SCHEMA_NAME.into(),
            field_definition_position: field_pos.clone().into(),
            is_part_of_provides: false,
        };
        let mut tails = query_graph
            .graph
            .edges_directed(head, Direction::Outgoing)
            .filter_map(|edge_ref| {
                let edge_weight = edge_ref.weight();
                if edge_weight.transition == expected_field_transition {
                    assert_eq!(edge_weight.conditions, None);
                    Some(edge_ref.target())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(tails.len(), 1);
        Ok(tails.pop().unwrap())
    }

    #[test]
    fn test_parse_context() {
        let fields = [
            ("$context { prop }", ("context", "{ prop }")),
            (
                "$context ... on A { prop } ... on B { prop }",
                ("context", "... on A { prop } ... on B { prop }"),
            ),
            (
                "$topLevelQuery { me { locale } }",
                ("topLevelQuery", "{ me { locale } }"),
            ),
            (
                "$context { a { b { c { prop }}} }",
                ("context", "{ a { b { c { prop }}} }"),
            ),
            (
                "$ctx { identifiers { legacyUserId } }",
                ("ctx", "{ identifiers { legacyUserId } }"),
            ),
            (
                "$retailCtx { identifiers { id5 } }",
                ("retailCtx", "{ identifiers { id5 } }"),
            ),
            ("$retailCtx { mid }", ("retailCtx", "{ mid }")),
            (
                "$widCtx { identifiers { wid } }",
                ("widCtx", "{ identifiers { wid } }"),
            ),
        ];
        for (field, (known_context, known_selection)) in fields {
            let (context, selection) = super::parse_context(field).unwrap();
            assert_eq!(context, known_context);
            assert_eq!(selection, known_selection);
        }
        // Ensure we don't backtrack in the comment regex.
        assert!(super::parse_context("#comment $fakeContext fakeSelection").is_err());
        assert!(super::parse_context("$ #comment fakeContext fakeSelection").is_err());
    }

    #[test]
    fn building_query_graphs_from_schema_handles_object_types() -> Result<(), FederationError> {
        let query_graph = test_query_graph_from_schema_sdl(
            r#"
            type Query {
              t1: T1
            }

            type T1 {
              f1: Int
              f2: String
              f3: T2
            }

            type T2 {
              t: T1
            }
            "#,
        )?;

        // We have 3 object types and 2 scalars (Int and String)
        assert_eq!(query_graph.graph.node_count(), 5);
        assert_eq!(
            query_graph
                .root_kinds_to_nodes()?
                .keys()
                .cloned()
                .collect::<IndexSet<_>>(),
            IndexSet::from_iter([SchemaRootDefinitionKind::Query])
        );

        let root_node = query_graph
            .root_kinds_to_nodes()?
            .get(&SchemaRootDefinitionKind::Query)
            .unwrap();
        assert_node_type(
            &query_graph,
            *root_node,
            ObjectTypeDefinitionPosition {
                type_name: name!("Query"),
            }
            .into(),
            Some(SchemaRootDefinitionKind::Query),
        )?;
        assert_eq!(
            query_graph
                .graph
                .edges_directed(*root_node, Direction::Outgoing)
                .count(),
            2
        );
        let root_fields = named_edges(
            &query_graph,
            *root_node,
            IndexSet::from_iter([name!("__typename"), name!("t1")]),
        )?;

        let root_typename_tail = root_fields.get("__typename").unwrap();
        assert_node_type(
            &query_graph,
            *root_typename_tail,
            ScalarTypeDefinitionPosition {
                type_name: name!("String"),
            }
            .into(),
            None,
        )?;

        let t1_node = root_fields.get("t1").unwrap();
        assert_node_type(
            &query_graph,
            *t1_node,
            ObjectTypeDefinitionPosition {
                type_name: name!("T1"),
            }
            .into(),
            None,
        )?;
        assert_eq!(
            query_graph
                .graph
                .edges_directed(*t1_node, Direction::Outgoing)
                .count(),
            4
        );
        let t1_fields = named_edges(
            &query_graph,
            *t1_node,
            IndexSet::from_iter([name!("__typename"), name!("f1"), name!("f2"), name!("f3")]),
        )?;

        let t1_typename_tail = t1_fields.get("__typename").unwrap();
        assert_node_type(
            &query_graph,
            *t1_typename_tail,
            ScalarTypeDefinitionPosition {
                type_name: name!("String"),
            }
            .into(),
            None,
        )?;

        let t1_f1_tail = t1_fields.get("f1").unwrap();
        assert_node_type(
            &query_graph,
            *t1_f1_tail,
            ScalarTypeDefinitionPosition {
                type_name: name!("Int"),
            }
            .into(),
            None,
        )?;
        assert_eq!(
            query_graph
                .graph
                .edges_directed(*t1_f1_tail, Direction::Outgoing)
                .count(),
            0
        );

        let t1_f2_tail = t1_fields.get("f2").unwrap();
        assert_node_type(
            &query_graph,
            *t1_f2_tail,
            ScalarTypeDefinitionPosition {
                type_name: name!("String"),
            }
            .into(),
            None,
        )?;
        assert_eq!(
            query_graph
                .graph
                .edges_directed(*t1_f2_tail, Direction::Outgoing)
                .count(),
            0
        );

        let t2_node = t1_fields.get("f3").unwrap();
        assert_node_type(
            &query_graph,
            *t2_node,
            ObjectTypeDefinitionPosition {
                type_name: name!("T2"),
            }
            .into(),
            None,
        )?;
        assert_eq!(
            query_graph
                .graph
                .edges_directed(*t2_node, Direction::Outgoing)
                .count(),
            2
        );
        let t2_fields = named_edges(
            &query_graph,
            *t2_node,
            IndexSet::from_iter([name!("__typename"), name!("t")]),
        )?;

        let t2_typename_tail = t2_fields.get("__typename").unwrap();
        assert_node_type(
            &query_graph,
            *t2_typename_tail,
            ScalarTypeDefinitionPosition {
                type_name: name!("String"),
            }
            .into(),
            None,
        )?;

        let t2_t_tail = t2_fields.get("t").unwrap();
        assert_node_type(
            &query_graph,
            *t2_t_tail,
            ObjectTypeDefinitionPosition {
                type_name: name!("T1"),
            }
            .into(),
            None,
        )?;

        Ok(())
    }
}
