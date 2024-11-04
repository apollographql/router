use std::sync::Arc;

use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::DirectiveList as ComponentDirectiveList;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use multimap::MultiMap;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use regex::Regex;
use strum::IntoEnumIterator;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::KeyDirectiveArguments;
use crate::operation::merge_selection_sets;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::query_graph::ContextCondition;
use crate::query_graph::OverrideCondition;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdge;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNode;
use crate::query_graph::QueryGraphNodeType;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::SchemaRootDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::subgraph::spec::CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::FROM_CONTEXT_DIRECTIVE_NAME;
use crate::supergraph::extract_subgraphs_from_supergraph;
use crate::utils::FallibleIterator;

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
        types_to_nodes_by_source: Default::default(),
        root_kinds_to_nodes_by_source: Default::default(),
        non_trivial_followup_edges: Default::default(),
        subgraph_to_args: Default::default(),
        subgraph_to_arg_indices: Default::default(),
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
        types_to_nodes_by_source: Default::default(),
        root_kinds_to_nodes_by_source: Default::default(),
        non_trivial_followup_edges: Default::default(),
        subgraph_to_args: Default::default(),
        subgraph_to_arg_indices: Default::default(),
    };
    let builder = SchemaQueryGraphBuilder::new(query_graph, name, schema, None, false)?;
    query_graph = builder.build()?;
    Ok(query_graph)
}

struct BaseQueryGraphBuilder {
    query_graph: QueryGraph,
}

impl BaseQueryGraphBuilder {
    fn new(mut query_graph: QueryGraph, source: Arc<str>, schema: ValidFederationSchema) -> Self {
        query_graph.current_source = source.clone();
        query_graph.sources.insert(source.clone(), schema);
        query_graph
            .types_to_nodes_by_source
            .insert(source.clone(), IndexMap::default());
        query_graph
            .root_kinds_to_nodes_by_source
            .insert(source.clone(), IndexMap::default());
        Self { query_graph }
    }

    fn build(self) -> QueryGraph {
        self.query_graph
    }

    fn add_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        transition: QueryGraphEdgeTransition,
        conditions: Option<Arc<SelectionSet>>,
    ) -> Result<(), FederationError> {
        self.query_graph
            .graph
            .add_edge(head, tail, QueryGraphEdge::new(transition, conditions));
        let head_weight = self.query_graph.node_weight(head)?;
        let tail_weight = self.query_graph.node_weight(tail)?;
        if head_weight.source != tail_weight.source {
            self.mark_has_reachable_cross_subgraph_edges_for_ancestors(head)?;
        }
        Ok(())
    }

    fn mark_has_reachable_cross_subgraph_edges_for_ancestors(
        &mut self,
        from: NodeIndex,
    ) -> Result<(), FederationError> {
        let from_weight = self.query_graph.node_weight(from)?;
        // When we mark a node, we mark all of its "ancestor" nodes, so if we get a node already
        // marked, there is nothing more to do.
        if from_weight.has_reachable_cross_subgraph_edges {
            return Ok(());
        }
        let mut stack = vec![from];
        while let Some(next) = stack.pop() {
            let next_weight = self.query_graph.node_weight_mut(next)?;
            next_weight.has_reachable_cross_subgraph_edges = true;
            let next_weight = self.query_graph.node_weight(next)?;
            for head in self
                .query_graph
                .graph
                .neighbors_directed(next, Direction::Incoming)
            {
                let head_weight = self.query_graph.node_weight(head)?;
                // Again, no point in redoing work as soon as we read an already-marked node. We
                // also only follow in-edges within the same subgraph, as nodes on other subgraphs
                // will have been marked with their own cross-subgraph edges.
                if head_weight.source == next_weight.source
                    && !head_weight.has_reachable_cross_subgraph_edges
                {
                    stack.push(head);
                }
            }
        }
        Ok(())
    }

    fn create_new_node(&mut self, type_: QueryGraphNodeType) -> Result<NodeIndex, FederationError> {
        let node = self.query_graph.graph.add_node(QueryGraphNode {
            type_: type_.clone(),
            source: self.query_graph.current_source.clone(),
            has_reachable_cross_subgraph_edges: false,
            provide_id: None,
            root_kind: None,
        });
        if let QueryGraphNodeType::SchemaType(pos) = type_ {
            self.query_graph
                .types_to_nodes_mut()?
                .entry(pos.type_name().clone())
                .or_default()
                .insert(node);
        }
        Ok(node)
    }

    fn create_root_node(
        &mut self,
        type_: QueryGraphNodeType,
        root_kind: SchemaRootDefinitionKind,
    ) -> Result<NodeIndex, FederationError> {
        let node = self.create_new_node(type_)?;
        self.set_as_root(node, root_kind)?;
        Ok(node)
    }

    fn set_as_root(
        &mut self,
        node: NodeIndex,
        root_kind: SchemaRootDefinitionKind,
    ) -> Result<(), FederationError> {
        let node_weight = self.query_graph.node_weight_mut(node)?;
        node_weight.root_kind = Some(root_kind);
        let root_kinds_to_nodes = self.query_graph.root_kinds_to_nodes_mut()?;
        root_kinds_to_nodes.insert(root_kind, node);
        Ok(())
    }
}

struct SchemaQueryGraphBuilder {
    base: BaseQueryGraphBuilder,
    subgraph: Option<SchemaQueryGraphBuilderSubgraphData>,
    for_query_planning: bool,
}

struct SchemaQueryGraphBuilderSubgraphData {
    federation_spec_definition: &'static FederationSpecDefinition,
    api_schema: ValidFederationSchema,
}

impl SchemaQueryGraphBuilder {
    /// If api_schema is given, then this builder assumes the schema is a subgraph schema, and that
    /// a subgraph query graph is being built.
    fn new(
        query_graph: QueryGraph,
        source: Arc<str>,
        schema: ValidFederationSchema,
        api_schema: Option<ValidFederationSchema>,
        for_query_planning: bool,
    ) -> Result<Self, FederationError> {
        let subgraph = if let Some(api_schema) = api_schema {
            let federation_spec_definition = get_federation_spec_definition_from_subgraph(&schema)?;
            Some(SchemaQueryGraphBuilderSubgraphData {
                federation_spec_definition,
                api_schema,
            })
        } else {
            None
        };
        let base = BaseQueryGraphBuilder::new(query_graph, source, schema);
        Ok(SchemaQueryGraphBuilder {
            base,
            subgraph,
            for_query_planning,
        })
    }

    fn build(mut self) -> Result<QueryGraph, FederationError> {
        // PORT_NOTE: Note that most of the JS code's buildGraphInternal() logic was moved into this
        // build() method.
        for root_kind in SchemaRootDefinitionKind::iter() {
            let pos = SchemaRootDefinitionPosition { root_kind };
            if pos
                .try_get(self.base.query_graph.schema()?.schema())
                .is_some()
            {
                self.add_recursively_from_root(pos)?;
            }
        }
        if self.subgraph.is_some() {
            self.add_interface_entity_edges()?;
        }
        if self.for_query_planning {
            self.add_additional_abstract_type_edges()?;
        }
        Ok(self.base.build())
    }

    fn is_external(
        &self,
        field_definition_position: &FieldDefinitionPosition,
    ) -> Result<bool, FederationError> {
        if let Some(subgraph_metadata) = self.base.query_graph.schema()?.subgraph_metadata() {
            Ok(subgraph_metadata
                .external_metadata()
                .is_external(field_definition_position))
        } else {
            Ok(false)
        }
    }

    /// Adds a node for the provided root object type (marking that node as a root node for the
    /// provided `kind`) and recursively descends into the type definition to add the related nodes
    /// and edges.
    ///
    /// In other words, calling this method on, say, the root query type of a schema will add nodes
    /// and edges for all the types reachable from that root query type.
    fn add_recursively_from_root(
        &mut self,
        root: SchemaRootDefinitionPosition,
    ) -> Result<(), FederationError> {
        let root_type_name = root.get(self.base.query_graph.schema()?.schema())?;
        let pos = match self
            .base
            .query_graph
            .schema()?
            .get_type(root_type_name.name.clone())?
        {
            TypeDefinitionPosition::Object(pos) => pos,
            _ => {
                return Err(SingleFederationError::Internal {
                    message: format!(
                        "Root type \"{}\" was unexpectedly not an object type",
                        root_type_name.name,
                    ),
                }
                .into());
            }
        };
        let node = self.add_type_recursively(pos.into())?;
        self.base.set_as_root(node, root.root_kind)
    }

    /// Adds in a node for the provided type in the in-building query graph, and recursively adds
    /// edges and nodes corresponding to the type definition (so for object types, it will add edges
    /// for each field and recursively add nodes for each field's type, etc...).
    fn add_type_recursively(
        &mut self,
        output_type_definition_position: OutputTypeDefinitionPosition,
    ) -> Result<NodeIndex, FederationError> {
        let type_name = output_type_definition_position.type_name().clone();
        if let Some(existing) = self.base.query_graph.types_to_nodes()?.get(&type_name) {
            if let Some(first_node) = existing.first() {
                return if existing.len() == 1 {
                    Ok(*first_node)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!(
                            "Only one node should have been created for type \"{}\", got {}",
                            type_name,
                            existing.len(),
                        ),
                    }
                    .into())
                };
            }
        }
        let node = self
            .base
            .create_new_node(output_type_definition_position.clone().into())?;
        match output_type_definition_position {
            OutputTypeDefinitionPosition::Object(pos) => {
                self.add_object_type_edges(pos, node)?;
            }
            OutputTypeDefinitionPosition::Interface(pos) => {
                // For interfaces, we generally don't add direct edges for their fields. Because in
                // general, the subgraph where a particular field can be fetched from may depend on
                // the runtime implementation. However, if the subgraph we're currently including
                // "provides" a particular interface field locally *for all the supergraph
                // interface's implementations* (in other words, we know we can always ask the field
                // to that subgraph directly on the interface and will never miss anything), then we
                // can add a direct edge to the field for the interface in that subgraph (which
                // avoids unnecessary type exploding in practice).
                if self.subgraph.is_some() {
                    self.maybe_add_interface_fields_edges(pos.clone(), node)?;
                }
                self.add_abstract_type_edges(pos.clone().into(), node)?;
            }
            OutputTypeDefinitionPosition::Union(pos) => {
                // Add the special-case __typename edge for unions.
                self.add_edge_for_field(pos.introspection_typename_field().into(), node, false)?;
                self.add_abstract_type_edges(pos.clone().into(), node)?;
            }
            // Any other case (scalar or enum; input objects are not possible here) is terminal and
            // has no edges to consider.
            _ => {}
        }
        Ok(node)
    }

    fn add_object_type_edges(
        &mut self,
        object_type_definition_position: ObjectTypeDefinitionPosition,
        head: NodeIndex,
    ) -> Result<(), FederationError> {
        let type_ =
            object_type_definition_position.get(self.base.query_graph.schema()?.schema())?;
        let is_interface_object = if let Some(subgraph) = &self.subgraph {
            let interface_object_directive_definition = subgraph
                .federation_spec_definition
                .interface_object_directive_definition(self.base.query_graph.schema()?)?
                .ok_or_else(|| SingleFederationError::Internal {
                    message: "Interface object directive definition unexpectedly missing"
                        .to_owned(),
                })?;
            type_
                .directives
                .iter()
                .any(|d| d.name == interface_object_directive_definition.name)
        } else {
            false
        };

        // Add edges to the query graph for each field. Note subgraph extraction adds the _entities
        // field to subgraphs, so when we recursively handle that field on the root query type, we
        // ensure that all entities are part of the graph (even if they are not reachable by any
        // other user operations).
        //
        // Note that FederationSchema ensures there are no introspection fields in "fields", but
        // we do handle __typename as a special case below.
        let fields = type_.fields.keys().cloned().collect::<Vec<_>>();
        for field_name in fields {
            // Fields marked @external only exist to ensure subgraph schemas are valid GraphQL, but
            // they don't create actual edges. However, even if we don't add an edge, we still want
            // to add the field's type. The reason is that while we don't add a "general" edge for
            // an external field, we may later add path-specific edges for the field due to a
            // `@provides`. When we do so, we need the node corresponding to that field type to
            // exist, and in rare cases a type could be only mentioned in this external field, so if
            // we don't add the type here, we never do and get issues later when we add @provides
            // edges.
            let pos = object_type_definition_position.field(field_name);
            let is_external = self.is_external(&pos.clone().into())?;
            self.add_edge_for_field(pos.into(), head, is_external)?;
        }
        // We add an edge for the built-in __typename field. For instance, it's perfectly valid to
        // query __typename manually, so we want to have an edge for it.
        //
        // However, note that @interfaceObject types are an exception to the rule of "it's perfectly
        // valid to query __typename". More precisely, a query can ask for the `__typename` of
        // anything, but it shouldn't be answered by an @interfaceObject and so we don't add an
        // edge in that case, ensuring the query planner has to get it from another subgraph (than
        // the one with said @interfaceObject).
        if !is_interface_object {
            let pos = object_type_definition_position.introspection_typename_field();
            self.add_edge_for_field(pos.into(), head, false)?;
        }

        Ok(())
    }

    fn add_edge_for_field(
        &mut self,
        field_definition_position: FieldDefinitionPosition,
        head: NodeIndex,
        skip_edge: bool,
    ) -> Result<(), FederationError> {
        let field = field_definition_position.get(self.base.query_graph.schema()?.schema())?;
        let tail_pos: OutputTypeDefinitionPosition = self
            .base
            .query_graph
            .schema()?
            .get_type(field.ty.inner_named_type().clone())?
            .try_into()?;
        let tail = self.add_type_recursively(tail_pos)?;
        if !skip_edge {
            let transition = QueryGraphEdgeTransition::FieldCollection {
                source: self.base.query_graph.current_source.clone(),
                field_definition_position,
                is_part_of_provides: false,
            };
            self.base.add_edge(head, tail, transition, None)?;
        }
        Ok(())
    }

    fn maybe_add_interface_fields_edges(
        &mut self,
        interface_type_definition_position: InterfaceTypeDefinitionPosition,
        head: NodeIndex,
    ) -> Result<(), FederationError> {
        let Some(subgraph) = &self.subgraph else {
            return Err(SingleFederationError::Internal {
                message: "Missing subgraph data when building subgraph query graph".to_owned(),
            }
            .into());
        };
        // In theory, the interface might have been marked inaccessible and not be in the API
        // schema. If that's the case, we just don't add direct edges at all (adding interface edges
        // is an optimization and if the interface is inaccessible, it probably doesn't play any
        // role in query planning anyway, so it doesn't matter).
        if interface_type_definition_position
            .try_get(subgraph.api_schema.schema())
            .is_none()
        {
            return Ok(());
        }

        let api_runtime_type_positions = subgraph
            .api_schema
            .referencers()
            .get_interface_type(&interface_type_definition_position.type_name)?
            .object_types
            .clone();
        // Note that it's possible that the current subgraph does not even know some of the possible
        // runtime types of the API schema. But as edges to interfaces can only come from the
        // current subgraph, it does mean that whatever field led to this interface was resolved in
        // this subgraph and can never return one of those unknown runtime types. So we can ignore
        // them.
        //
        // TODO: We *must* revisit this once we fully add @key for interfaces as it will invalidate
        // the "edges to interfaces can only come from the current subgraph". Most likely, _if_ an
        // interface has a key, then we should return early from this function (add no field edges
        // at all) if the subgraph doesn't know of at least one implementation.
        let mut local_runtime_type_positions = Vec::new();
        for api_runtime_type_position in &api_runtime_type_positions {
            if api_runtime_type_position
                .try_get(self.base.query_graph.schema()?.schema())
                .is_some()
            {
                local_runtime_type_positions.push(api_runtime_type_position)
            }
        }
        let type_ =
            interface_type_definition_position.get(self.base.query_graph.schema()?.schema())?;

        // Same as for objects, we add edges to the query graph for each field.
        //
        // Note that FederationSchema ensures there are no introspection fields in "fields", but
        // we do handle __typename as a special case below.
        let fields = type_.fields.keys().cloned().collect::<Vec<_>>();
        for field_name in fields {
            // To include the field, it must not be external itself, and it must be provided on
            // all of the local runtime types.
            let pos = interface_type_definition_position.field(field_name.clone());
            let is_external = self.is_external(&pos.clone().into())?;
            let mut is_provided_by_all_local_types = true;
            for local_runtime_type in &local_runtime_type_positions {
                if !self
                    .is_directly_provided_by_type(local_runtime_type.field(field_name.clone()))?
                {
                    is_provided_by_all_local_types = false;
                }
            }
            if is_external || !is_provided_by_all_local_types {
                continue;
            }
            self.add_edge_for_field(pos.into(), head, false)?;
        }
        // Same as for objects, we add an edge for the built-in __typename field.
        //
        // Note that __typename will never be external and will always provided by all local runtime
        // types, so we unconditionally add the edge here.
        self.add_edge_for_field(
            interface_type_definition_position
                .introspection_typename_field()
                .into(),
            head,
            false,
        )
    }

    fn is_directly_provided_by_type(
        &self,
        object_field_definition_position: ObjectFieldDefinitionPosition,
    ) -> Result<bool, FederationError> {
        // The field is directly provided if:
        //   1) the type does have it.
        //   2) it is not external.
        //   3) it does not have a @requires (essentially, this method is called on type
        //      implementations of an interface to decide if we can avoid type-explosion, but if the
        //      field has a @requires on an implementation, then we need to type-explode to make
        //      sure we handle that @requires).
        if object_field_definition_position
            .try_get(self.base.query_graph.schema()?.schema())
            .is_none()
        {
            return Ok(false);
        }
        let is_external = self.is_external(&object_field_definition_position.clone().into())?;
        let has_requires = if let Some(subgraph) = &self.subgraph {
            let requires_directive_definition = subgraph
                .federation_spec_definition
                .requires_directive_definition(self.base.query_graph.schema()?)?;
            object_field_definition_position
                .get(self.base.query_graph.schema()?.schema())?
                .directives
                .iter()
                .any(|d| d.name == requires_directive_definition.name)
        } else {
            false
        };
        Ok(!is_external && !has_requires)
    }

    fn add_abstract_type_edges(
        &mut self,
        abstract_type_definition_position: AbstractTypeDefinitionPosition,
        head: NodeIndex,
    ) -> Result<(), FederationError> {
        let implementations = self
            .base
            .query_graph
            .schema()?
            .possible_runtime_types(abstract_type_definition_position.clone().into())?;
        for pos in implementations {
            let tail = self.add_type_recursively(pos.clone().into())?;
            let transition = QueryGraphEdgeTransition::Downcast {
                source: self.base.query_graph.current_source.clone(),
                from_type_position: abstract_type_definition_position.clone().into(),
                to_type_position: pos.into(),
            };
            self.base.add_edge(head, tail, transition, None)?;
        }
        Ok(())
    }

    /// We've added edges that avoid type-explosion _directly_ from an interface, but it means that
    /// so far we always type-explode unions to all their implementation types, and always
    /// type-explode when we go through 2 unrelated interfaces. For instance, say we have
    /// ```graphql
    /// type Query {
    ///   i1: I1
    ///   i2: I2
    ///   u: U
    /// }
    ///
    /// interface I1 {
    ///   x: Int
    /// }
    ///
    /// interface I2 {
    ///   y: Int
    /// }
    ///
    /// type A implements I1 & I2 {
    ///   x: Int
    ///   y: Int
    /// }
    ///
    /// type B implements I1 & I2 {
    ///   x: Int
    ///   y: Int
    /// }
    ///
    /// union U = A | B
    /// ```
    /// If we query:
    /// ```graphql
    /// {
    ///   u {
    ///     ... on I1 {
    ///       x
    ///     }
    ///   }
    /// }
    /// ```
    /// then we currently have no edge between `U` and `I1` whatsoever, so query planning would have
    /// to type-explode `U` even though that's not necessary (assuming everything is in the same
    /// subgraph, we'd want to send the query "as-is").
    /// Same thing for:
    /// ```graphql
    /// {
    ///   i1 {
    ///     x
    ///     ... on I2 {
    ///       y
    ///     }
    ///   }
    /// }
    /// ```
    /// due to not having edges from `I1` to `I2` (granted, in that example, type-exploding is not
    /// all that worse, but it gets worse with more implementations/fields).
    ///
    /// And so this method is about adding such edges. Essentially, every time 2 abstract types have
    /// an intersection of runtime types > 1, we add an edge.
    ///
    /// Do note that in practice we only add those edges when we build a query graph for query
    /// planning purposes, because not type-exploding is only an optimization but type-exploding
    /// will always "work" and for composition validation, we don't care about being optimal, while
    /// limiting edges make validation faster by limiting the choices to explore. Also, query
    /// planning is careful, as it walks those edges, to compute the actual possible runtime types
    /// we could have to avoid later type-exploding in impossible runtime types.
    fn add_additional_abstract_type_edges(&mut self) -> Result<(), FederationError> {
        // As mentioned above, we only care about this on subgraph query graphs during query
        // planning. But if this ever gets called in some other code path, ignore this.
        let Some(subgraph) = &self.subgraph else {
            return Ok(());
        };

        // For each abstract type in the schema, compute its runtime types.
        let mut abstract_types_with_runtime_types = Vec::new();
        for (type_name, type_) in &self.base.query_graph.schema()?.schema().types {
            let pos: AbstractTypeDefinitionPosition = match type_ {
                ExtendedType::Interface(_) => InterfaceTypeDefinitionPosition {
                    type_name: type_name.clone(),
                }
                .into(),
                ExtendedType::Union(_) => UnionTypeDefinitionPosition {
                    type_name: type_name.clone(),
                }
                .into(),
                _ => continue,
            };
            // All "normal" types from subgraphs should be in the API schema, but there are a
            // couple exceptions:
            // - Subgraphs have the `_Entity` type, which is not in the API schema.
            // - Types marked @inaccessible also won't be in the API schema.
            // In those cases, we don't create any additional edges for those types. For
            // inaccessible types, we could theoretically try to add them, but we would need the
            // full supergraph while we currently only have access to the API schema, and besides,
            // inaccessible types can only be part of the query execution in indirect ways (e.g.
            // through some @requires), and you'd need pretty weird @requires for the
            // optimization here to ever matter.
            match subgraph.api_schema.schema().types.get(type_name) {
                Some(ExtendedType::Interface(_)) => {}
                Some(ExtendedType::Union(_)) => {}
                None => continue,
                _ => {
                    return Err(SingleFederationError::Internal {
                        message: format!(
                            "Type \"{}\" was abstract in subgraph but not in API schema",
                            type_name,
                        ),
                    }
                    .into());
                }
            }
            abstract_types_with_runtime_types.push(AbstractTypeWithRuntimeTypes {
                abstract_type_definition_position: pos.clone(),
                subgraph_runtime_type_positions: self
                    .base
                    .query_graph
                    .schema()?
                    .possible_runtime_types(pos.clone().into())?,
                api_runtime_type_positions: subgraph
                    .api_schema
                    .possible_runtime_types(pos.clone().into())?,
            });
        }

        // Check every pair of abstract types that intersect on at least 2 runtime types to see if
        // we have edges to add. Note that in practice, we only care about 'Union -> Interface' and
        // 'Interface -> Interface'.
        for (i, t1) in abstract_types_with_runtime_types.iter().enumerate() {
            // Note that in general, t1 is already part of the graph, so `add_type_recursively()`
            // doesn't really add anything, it just returns the existing node. That said, if t1 is
            // returned by no field (at least no field reachable from a root type), that type will
            // not be part of the graph. And in that case, we do add it. And it's actually
            // possible that we don't create any edge to that created node, so we may be creating a
            // disconnected subset of the graph, a part that is not reachable from any root. It's
            // not optimal, but it's a bit hard to avoid in the first place (we could also try to
            // purge such subsets after this method, but it's probably not worth it in general) and
            // it's not a big deal: it will just use a bit more memory than necessary, and it's
            // probably pretty rare in the first place.
            let t1_node =
                self.add_type_recursively(t1.abstract_type_definition_position.clone().into())?;
            for (j, t2) in abstract_types_with_runtime_types.iter().enumerate() {
                if j > i {
                    break;
                }

                // PORT_NOTE: The JS code skipped cases where interfaces implemented other
                // interfaces, claiming this was handled already by add_abstract_type_edges().
                // However, this was not actually handled by that function, so we fix the bug here
                // by not early-returning for interfaces implementing interfaces.

                let mut add_t1_to_t2 = false;
                let mut add_t2_to_t1 = false;
                if t1.abstract_type_definition_position == t2.abstract_type_definition_position {
                    // We always add an edge from a type to itself. This is just saying that if
                    // we're type-casting to the type we're already on, it's doing nothing, and in
                    // particular it shouldn't force us to type-explode anymore that if we didn't
                    // have the cast in the first place. Note that we only set `add_t1_To_t2` to
                    // true, otherwise we'd be adding the same edge twice.
                    add_t1_to_t2 = true;
                } else {
                    // Otherwise, there is 2 aspects to take into account:
                    // - It's only worth adding an edge between types, meaning that we might save
                    //   type-exploding into the runtime types of the target/"to" one, if the local
                    //   intersection (of runtime types, in the current subgraph) for the abstract
                    //   types is more than 2. If it's just 1 type, then going to that type directly
                    //   is not less efficient and is more precise in a sense. And if the
                    //   intersection is empty, then no point in polluting the query graphs with
                    //   edges we'll never take.
                    // - _But_ we can only save type-exploding if that local intersection does not
                    //   exclude any runtime types that are local to the source/"from" type, not
                    //   local to the target/"to" type, *but* are global to the target/"to" type,
                    //   because such types should not be excluded and only type-explosion will
                    //   achieve that (for some concrete examples, see the "merged abstract types
                    //   handling" tests in `build_plan()` tests). In other words, we don't want to
                    //   avoid the type explosion if there is a type in the intersection of the
                    //   local source/"from" runtime types and global target/"to" runtime types that
                    //   is not in the purely local runtime type intersection.

                    let intersecting_local_runtime_type_positions = t1
                        .subgraph_runtime_type_positions
                        .intersection(&t2.subgraph_runtime_type_positions)
                        .collect::<IndexSet<_>>();
                    if intersecting_local_runtime_type_positions.len() >= 2 {
                        let is_in_local_other_type_but_not_local_intersection = |type_pos: &ObjectTypeDefinitionPosition, other_type: &AbstractTypeWithRuntimeTypes| {
                            other_type.subgraph_runtime_type_positions.contains(type_pos) &&
                                !intersecting_local_runtime_type_positions.contains(type_pos)
                        };
                        // TODO: we're currently _never_ adding the edge if the target/"to" type is
                        // a union. We shouldn't be doing that, this will genuinely make some cases
                        // less efficient than they could be (though those cases are admittedly a
                        // bit convoluted), but this make sense *until*
                        // https://github.com/apollographql/federation/issues/2256 gets fixed.
                        // Because until then, we do not properly track unions through composition,
                        // and that means there is never a difference (in the query planner) between
                        // a local union definition and the supergraph one, even if that different
                        // actually exists. And so, never type-exploding in that case is somewhat
                        // safer, as not-type-exploding is ultimately an optimisation. Please note
                        // that this is *not* a fix for #2256, and most of the issues created by
                        // #2256 still needs fixing, but it avoids making it even worth for a few
                        // corner cases. We should remove the `isUnionType` below once the
                        // fix for #2256 is implemented.
                        if !(matches!(
                            t2.abstract_type_definition_position,
                            AbstractTypeDefinitionPosition::Union(_)
                        ) || t2
                            .api_runtime_type_positions
                            .iter()
                            .any(|rt| is_in_local_other_type_but_not_local_intersection(rt, t1)))
                        {
                            add_t1_to_t2 = true;
                        }
                        if !(matches!(
                            t1.abstract_type_definition_position,
                            AbstractTypeDefinitionPosition::Union(_)
                        ) || t1
                            .api_runtime_type_positions
                            .iter()
                            .any(|rt| is_in_local_other_type_but_not_local_intersection(rt, t2)))
                        {
                            add_t2_to_t1 = true;
                        }
                    }
                }

                if add_t1_to_t2 || add_t2_to_t1 {
                    // Same remark as for t1 above.
                    let t2_node = self.add_type_recursively(
                        t2.abstract_type_definition_position.clone().into(),
                    )?;
                    if add_t1_to_t2 {
                        let transition = QueryGraphEdgeTransition::Downcast {
                            source: self.base.query_graph.current_source.clone(),
                            from_type_position: t1.abstract_type_definition_position.clone().into(),
                            to_type_position: t2.abstract_type_definition_position.clone().into(),
                        };
                        self.base.add_edge(t1_node, t2_node, transition, None)?;
                    }
                    if add_t2_to_t1 {
                        let transition = QueryGraphEdgeTransition::Downcast {
                            source: self.base.query_graph.current_source.clone(),
                            from_type_position: t2.abstract_type_definition_position.clone().into(),
                            to_type_position: t1.abstract_type_definition_position.clone().into(),
                        };
                        self.base.add_edge(t2_node, t1_node, transition, None)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// In a subgraph, all entity object types will be "automatically" reachable (from the root
    /// query type) because of the `_entities` field (it returns `_Entity`, which is a union of all
    /// entity object types, making those reachable.
    ///
    /// However, we also want entity interface types (interfaces with an @key) to be reachable in a
    /// similar way, because the `_entities` field is also technically the one resolving them, and
    /// not having them reachable would break plenty of code that assume that by traversing a query
    /// graph from root, we get to everything that can be queried.
    ///
    /// But because GraphQL unions cannot have interface types, they are not part of the `_Entity`
    /// union (and cannot be). This is ok as far as the typing of the schema goes, because even when
    /// `_entities` is called to resolve an interface type, it technically returns a concrete
    /// object, and so, since every implementation of an entity interface is also an entity, this is
    /// captured by the `_Entity` union.
    ///
    /// But it does mean we want to manually add the corresponding edges now for interfaces, or @key
    /// on interfaces wouldn't work properly (at least, when the interface is not otherwise
    /// reachable by an operation on the subgraph).
    fn add_interface_entity_edges(&mut self) -> Result<(), FederationError> {
        let Some(subgraph) = &self.subgraph else {
            return Err(SingleFederationError::Internal {
                message: "Missing subgraph data when building subgraph query graph".to_owned(),
            }
            .into());
        };
        let federation_spec_definition = subgraph.federation_spec_definition;
        let entity_type_definition =
            federation_spec_definition.entity_type_definition(self.base.query_graph.schema()?)?;
        // We can ignore this case because if the subgraph has an interface with an @key, then we
        // force its implementations to be marked as entity too and so we know that if `_Entity` is
        // undefined, then we have no need for entity edges.
        let Some(entity_type_definition) = entity_type_definition else {
            return Ok(());
        };
        let entity_type_name = entity_type_definition.name.clone();
        let entity_type_node = self.add_type_recursively(
            UnionTypeDefinitionPosition {
                type_name: entity_type_name.clone(),
            }
            .into(),
        )?;
        let key_directive_definition =
            federation_spec_definition.key_directive_definition(self.base.query_graph.schema()?)?;
        let mut interface_type_definition_positions = Vec::new();
        for (type_name, type_) in &self.base.query_graph.schema()?.schema().types {
            let ExtendedType::Interface(type_) = type_ else {
                continue;
            };
            if !resolvable_key_applications(
                &type_.directives,
                &key_directive_definition.name,
                federation_spec_definition,
            )?
            .is_empty()
            {
                interface_type_definition_positions.push(InterfaceTypeDefinitionPosition {
                    type_name: type_name.clone(),
                });
            }
        }
        for interface_type_definition_position in interface_type_definition_positions {
            let interface_type_node =
                self.add_type_recursively(interface_type_definition_position.clone().into())?;
            let transition = QueryGraphEdgeTransition::Downcast {
                source: self.base.query_graph.current_source.clone(),
                from_type_position: UnionTypeDefinitionPosition {
                    type_name: entity_type_name.clone(),
                }
                .into(),
                to_type_position: interface_type_definition_position.into(),
            };
            self.base
                .add_edge(entity_type_node, interface_type_node, transition, None)?;
        }

        Ok(())
    }
}

struct AbstractTypeWithRuntimeTypes {
    abstract_type_definition_position: AbstractTypeDefinitionPosition,
    subgraph_runtime_type_positions: IndexSet<ObjectTypeDefinitionPosition>,
    api_runtime_type_positions: IndexSet<ObjectTypeDefinitionPosition>,
}

struct FederatedQueryGraphBuilder {
    base: BaseQueryGraphBuilder,
    supergraph_schema: ValidFederationSchema,
    subgraphs: FederatedQueryGraphBuilderSubgraphs,
}

impl FederatedQueryGraphBuilder {
    fn new(
        query_graph: QueryGraph,
        supergraph_schema: ValidFederationSchema,
    ) -> Result<Self, FederationError> {
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
            .flat_map(|(_, root_kind_to_notes)| root_kind_to_notes.keys().copied())
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
        let mut edge_to_conditions: HashMap<EdgeIndex, OverrideCondition> = Default::default();

        fn collect_edge_condition(
            query_graph: &QueryGraph,
            target_graph: &str,
            target_field: &ObjectFieldDefinitionPosition,
            label: &str,
            condition: bool,
            edge_to_conditions: &mut HashMap<EdgeIndex, OverrideCondition>,
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
        fn parse_context(field: &str) -> Result<(String, String), FederationError> {
            let pattern = Regex::new(
                r#"/^(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*\$(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*([A-Za-z_]\w*(?!\w))([\s\S]*)$/"#,
            ).unwrap();

            let mut iter = pattern.find_iter(field);

            let Some(conext) = iter.next() else {
                internal_error!("Expected 'context' in @fromContext(field) not found");
            };

            let Some(selection) = iter.next() else {
                internal_error!("Expected 'selection' in @fromContext(field) not found");
            };

            if iter.next().is_some() {
                internal_error!("More than 'context' and 'selection' found in @fromContext(field)");
            }

            Ok((conext.as_str().to_owned(), selection.as_str().to_owned()))
        }

        let mut subgraph_to_args: MultiMap<Arc<str>, ObjectFieldArgumentDefinitionPosition> =
            MultiMap::new();
        for (source, subgraph) in self.base.query_graph.subgraphs() {
            let Some((_, context_refs)) = &subgraph
                .referencers()
                .directives
                .iter()
                // TODO: I think I need to get the subgraph data which contains the name of the
                // context in that subgraph, ya?
                .find(|(dir, _)| **dir == CONTEXT_DIRECTIVE_NAME)
            else {
                continue;
            };

            let Some((_, from_context_refs)) = &subgraph
                .referencers()
                .directives
                .iter()
                // TODO: I think I need to get the subgraph data which contains the name of
                // fromContext in that subgraph, ya?
                .find(|(dir, _)| **dir == FROM_CONTEXT_DIRECTIVE_NAME)
            else {
                continue;
            };

            // For @context
            // subgraph -> referencers -> iter through object_types and interface_types
            let subgraph_data = self.subgraphs.get(source)?;
            let mut context_name_to_types: HashMap<&str, HashSet<&ObjectTypeDefinitionPosition>> =
                HashMap::default();
            for object_def_pos in &context_refs.object_types {
                let object = object_def_pos.get(subgraph.schema())?;
                for dir in object.directives.get_all(&CONTEXT_DIRECTIVE_NAME) {
                    let application = FederationSpecDefinition::context_directive_arguments(dir)?;
                    context_name_to_types
                        .entry(application.name)
                        .or_default()
                        .insert(object_def_pos);
                }
            }

            // For @fromContext
            // subgraph -> referencers -> iter through object_field_arguments and interface_field_arguments
            let mut coordinate_map: HashMap<ObjectFieldDefinitionPosition, Vec<ContextCondition>> =
                HashMap::default();
            for object_field_arg in &from_context_refs.object_field_arguments {
                let input_value = object_field_arg.get(subgraph.schema())?;
                let coordinate = object_field_arg.parent();
                subgraph_to_args
                    .entry(source.clone())
                    .or_insert_vec(vec![object_field_arg.clone()]);
                for dir in input_value.directives.get_all(&FROM_CONTEXT_DIRECTIVE_NAME) {
                    let application = subgraph_data
                        .federation_spec_definition
                        .from_context_directive_arguments(dir)?;
                    let (context, selection) = parse_context(application.field)?;

                    let types_with_context_set = context_name_to_types
                        .get(context.as_str())
                        .into_iter()
                        .flatten()
                        .cloned()
                        .cloned()
                        .collect();
                    let conditions = ContextCondition {
                        context,
                        selection,
                        subgraph_name: source.clone(),
                        coordinate: coordinate.clone(),
                        types_with_context_set,
                    };
                    coordinate_map
                        .entry(coordinate.clone())
                        .or_default()
                        .push(conditions);
                }
            }
            // TODO:
            // Do the simpleTraversal

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
                // TODO: Port the check of `e.head.type.kind === 'ObjectType'`
                // I think that's found via the collection's field def pos
                if *source == self.base.query_graph.current_source {
                    continue;
                }
            }
        }

        // Add the context argument mapping
        let mut subgraph_to_arg_indices: HashMap<
            Arc<str>,
            HashMap<ObjectFieldArgumentDefinitionPosition, String>,
        > = HashMap::default();
        for (source, _) in self.base.query_graph.subgraphs() {
            if let Some(args) = subgraph_to_args.get_vec(source.as_ref()) {
                // The JS code sorted the list, but the ObjectFieldDefinitionPosition does
                // implement Ord. Can we ignore sorting?
                // args.sort();
                let args = args
                    .iter()
                    .enumerate()
                    .map(|(i, arg)| (arg.clone(), format!("contextualArgument__{source}_{i}")))
                    .collect();
                subgraph_to_arg_indices.insert(source.clone(), args);
            }
        }

        self.base.query_graph.subgraph_to_args = subgraph_to_args;
        self.base.query_graph.subgraph_to_arg_indices = subgraph_to_arg_indices;

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
                    Selection::FragmentSpread(_) => {
                        return Err(SingleFederationError::Internal {
                            message: "Unexpectedly found named fragment in FieldSet scalar"
                                .to_owned(),
                        }
                        .into());
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

fn resolvable_key_applications<'doc>(
    directives: &'doc ComponentDirectiveList,
    key_directive_definition_name: &Name,
    federation_spec_definition: &'static FederationSpecDefinition,
) -> Result<Vec<KeyDirectiveArguments<'doc>>, FederationError> {
    let mut applications = Vec::new();
    for directive in directives.get_all(key_directive_definition_name) {
        let key_directive_application =
            federation_spec_definition.key_directive_arguments(directive)?;
        if !key_directive_application.resolvable {
            continue;
        }
        applications.push(key_directive_application);
    }
    Ok(applications)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::collections::IndexSet;
    use apollo_compiler::name;
    use apollo_compiler::Name;
    use apollo_compiler::Schema;
    use petgraph::graph::NodeIndex;
    use petgraph::visit::EdgeRef;
    use petgraph::Direction;

    use crate::error::FederationError;
    use crate::query_graph::build_query_graph::build_query_graph;
    use crate::query_graph::QueryGraph;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::query_graph::QueryGraphNode;
    use crate::query_graph::QueryGraphNodeType;
    use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::schema::position::OutputTypeDefinitionPosition;
    use crate::schema::position::ScalarTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;
    use crate::schema::ValidFederationSchema;

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
