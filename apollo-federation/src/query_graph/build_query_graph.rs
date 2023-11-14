use crate::error::{FederationError, SingleFederationError};
use crate::link::federation_spec_definition::{
    FederationSpecDefinition, KeyDirectiveArguments, FEDERATION_VERSIONS,
};
use crate::link::spec::Identity;
use crate::link::spec_definition::spec_definitions;
use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
use crate::query_graph::{
    QueryGraph, QueryGraphEdge, QueryGraphEdgeTransition, QueryGraphNode, QueryGraphNodeType,
};
use crate::schema::position::{
    AbstractTypeDefinitionPosition, FieldDefinitionPosition, InterfaceTypeDefinitionPosition,
    ObjectFieldDefinitionPosition, ObjectTypeDefinitionPosition, OutputTypeDefinitionPosition,
    SchemaRootDefinitionKind, SchemaRootDefinitionPosition, TypeDefinitionPosition,
    UnionTypeDefinitionPosition,
};
use crate::schema::FederationSchema;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::{Component, Directive, ExtendedType, Name};
use apollo_compiler::{NodeStr, Schema};
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::NodeIndex;
use petgraph::Direction;
use std::ops::Deref;
use std::sync::Arc;
use strum::IntoEnumIterator;

// Builds a "federated" query graph based on the provided supergraph and API schema.
//
// A federated query graph is one that is used to reason about queries made by a router against a
// set of federated subgraph services.
//
// Assumes the given schemas have been validated.
pub fn build_federated_query_graph(
    supergraph_schema: Schema,
    api_schema: Arc<FederationSchema>,
    validate_extracted_subgraphs: Option<bool>,
    for_query_planning: Option<bool>,
) -> Result<QueryGraph, FederationError> {
    let for_query_planning = for_query_planning.unwrap_or(true);
    let mut query_graph = QueryGraph {
        // Note this name is a dummy initial name that gets overridden as we build the query graph.
        name: NodeStr::new(""),
        graph: Default::default(),
        sources: Default::default(),
        types_to_nodes_by_source: Default::default(),
        root_kinds_to_nodes_by_source: Default::default(),
        non_trivial_followup_edges: Default::default(),
    };
    let subgraphs =
        extract_subgraphs_from_supergraph(supergraph_schema, validate_extracted_subgraphs)?;
    for (subgraph_name, subgraph) in subgraphs {
        let federation_link = &subgraph
            .schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&Identity::federation_identity()))
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Subgraph unexpectedly does not use federation spec".to_owned(),
            })?;
        let federation_spec_definition = spec_definitions(FEDERATION_VERSIONS.deref())?
            .find(&federation_link.url.version)
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Subgraph unexpectedly does not use a supported federation spec version"
                    .to_owned(),
            })?;
        let builder = SchemaQueryGraphBuilder::new(
            query_graph,
            NodeStr::new(&subgraph_name),
            subgraph.schema,
            Some(SchemaQueryGraphBuilderSubgraphData {
                federation_spec_definition,
                api_schema: api_schema.clone(),
            }),
            for_query_planning,
        );
        query_graph = builder.build()?;
    }

    Ok(query_graph)
}
struct BaseQueryGraphBuilder {
    source: NodeStr,
    query_graph: QueryGraph,
}

impl BaseQueryGraphBuilder {
    fn new(mut query_graph: QueryGraph, source: NodeStr, schema: FederationSchema) -> Self {
        query_graph.name = source.clone();
        query_graph.sources.insert(source.clone(), schema);
        query_graph
            .types_to_nodes_by_source
            .insert(source.clone(), IndexMap::new());
        query_graph
            .root_kinds_to_nodes_by_source
            .insert(source.clone(), IndexMap::new());
        Self {
            source,
            query_graph,
        }
    }

    fn build(self) -> QueryGraph {
        self.query_graph
    }

    fn add_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        transition: QueryGraphEdgeTransition,
        conditions: Option<SelectionSet>,
    ) -> Result<(), FederationError> {
        self.query_graph.graph.add_edge(
            head,
            tail,
            QueryGraphEdge {
                transition,
                conditions,
            },
        );
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
            source: self.source.clone(),
            has_reachable_cross_subgraph_edges: false,
            provide_id: None,
            root_kind: None,
        });
        if let QueryGraphNodeType::SubgraphType(pos) = type_ {
            self.query_graph
                .types_to_nodes_mut()?
                .entry(pos.type_name().clone())
                .or_insert_with(IndexSet::new)
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
        node_weight.root_kind = Some(root_kind.clone());
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
    api_schema: Arc<FederationSchema>,
}

impl SchemaQueryGraphBuilder {
    fn new(
        query_graph: QueryGraph,
        source: NodeStr,
        schema: FederationSchema,
        subgraph: Option<SchemaQueryGraphBuilderSubgraphData>,
        for_query_planning: bool,
    ) -> Self {
        let base = BaseQueryGraphBuilder::new(query_graph, source, schema);
        SchemaQueryGraphBuilder {
            base,
            subgraph,
            for_query_planning,
        }
    }

    fn build(mut self) -> Result<QueryGraph, FederationError> {
        // PORT_NOTE: Note that most of the JS code's buildGraphInternal() logic was moved into this
        // build() method.
        for root_kind in SchemaRootDefinitionKind::iter() {
            let pos = SchemaRootDefinitionPosition {
                root_kind: root_kind.clone(),
            };
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
        Ok(self.base.query_graph)
    }

    fn is_external(
        &self,
        field_definition_position: FieldDefinitionPosition,
    ) -> Result<bool, FederationError> {
        // TODO: Should port JS ExternalTester for this, as we're missing some fields which are
        // effectively external.
        Ok(if let Some(subgraph) = &self.subgraph {
            let external_directive_definition = subgraph
                .federation_spec_definition
                .external_directive_definition(self.base.query_graph.schema()?)?;
            field_definition_position
                .get(self.base.query_graph.schema()?.schema())?
                .directives
                .iter()
                .any(|d| d.name == external_directive_definition.name)
        } else {
            false
        })
    }

    // Adds a node for the provided root object type (marking that node as a root node for the
    // provided `kind`) and recursively descends into the type definition to add the related nodes
    // and edges.
    //
    // In other words, calling this method on, say, the root query type of a schema will add nodes
    // and edges for all the types reachable from that root query type.
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
        self.base.set_as_root(node, root.root_kind.clone())
    }

    // Adds in a node for the provided type in the in-building query graph, and recursively adds
    // edges and nodes corresponding to the type definition (so for object types, it will add edges
    // for each field and recursively add nodes for each field's type, etc...).
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
        let node = self.base.create_new_node(QueryGraphNodeType::SubgraphType(
            output_type_definition_position.clone(),
        ))?;
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
            let is_external = self.is_external(pos.clone().into())?;
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
        let tail_pos = match self
            .base
            .query_graph
            .schema()?
            .get_type(field.ty.inner_named_type().clone())?
        {
            TypeDefinitionPosition::Scalar(pos) => pos.into(),
            TypeDefinitionPosition::Object(pos) => pos.into(),
            TypeDefinitionPosition::Interface(pos) => pos.into(),
            TypeDefinitionPosition::Union(pos) => pos.into(),
            TypeDefinitionPosition::Enum(pos) => pos.into(),
            TypeDefinitionPosition::InputObject(pos) => {
                return Err(SingleFederationError::Internal {
                    message: format!(
                        "Field \"{}\" has non-output type \"{}\"",
                        field_definition_position, pos,
                    ),
                }
                .into());
            }
        };
        let tail = self.add_type_recursively(tail_pos)?;
        let transition = QueryGraphEdgeTransition::FieldCollection {
            source: self.base.source.clone(),
            field_definition_position,
            is_part_of_provide: false,
        };
        if !skip_edge {
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
            let is_external = self.is_external(pos.clone().into())?;
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
        let is_external = self.is_external(object_field_definition_position.clone().into())?;
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
                source: self.base.source.clone(),
                from_type_position: abstract_type_definition_position.clone().into(),
                to_type_position: pos.into(),
            };
            self.base.add_edge(head, tail, transition, None)?;
        }
        Ok(())
    }

    // We've added edges that avoid type-explosion _directly_ from an interface, but it means that
    // so far we always type-explode unions to all their implementation types, and always
    // type-explode when we go through 2 unrelated interfaces. For instance, say we have
    // ```
    // type Query {
    //   i1: I1
    //   i2: I2
    //   u: U
    // }
    //
    // interface I1 {
    //   x: Int
    // }
    //
    // interface I2 {
    //   y: Int
    // }
    //
    // type A implements I1 & I2 {
    //   x: Int
    //   y: Int
    // }
    //
    // type B implements I1 & I2 {
    //   x: Int
    //   y: Int
    // }
    //
    // union U = A | B
    // ```
    // If we query:
    // ```
    // {
    //   u {
    //     ... on I1 {
    //       x
    //     }
    //   }
    // }
    // ```
    // then we currently have no edge between `U` and `I1` whatsoever, so query planning would have
    // to type-explode `U` even though that's not necessary (assuming everything is in the same
    // subgraph, we'd want to send the query "as-is").
    // Same thing for:
    // ```
    // {
    //   i1 {
    //     x
    //     ... on I2 {
    //       y
    //     }
    //   }
    // }
    // ```
    // due to not having edges from `I1` to `I2` (granted, in that example, type-exploding is not
    // all that worse, but it gets worse with more implementations/fields).
    //
    // And so this method is about adding such edges. Essentially, every time 2 abstract types have
    // an intersection of runtime types > 1, we add an edge.
    //
    // Do note that in practice we only add those edges when we build a query graph for query
    // planning purposes, because not type-exploding is only an optimization but type-exploding will
    // always "work" and for composition validation, we don't care about being optimal, while
    // limiting edges make validation faster by limiting the choices to explore. Also, query
    // planning is careful, as it walks those edges, to compute the actual possible runtime types we
    // could have to avoid later type-exploding in impossible runtime types.
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
                            source: self.base.source.clone(),
                            from_type_position: t1.abstract_type_definition_position.clone().into(),
                            to_type_position: t2.abstract_type_definition_position.clone().into(),
                        };
                        self.base.add_edge(t1_node, t2_node, transition, None)?;
                    }
                    if add_t2_to_t1 {
                        let transition = QueryGraphEdgeTransition::Downcast {
                            source: self.base.source.clone(),
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

    // In a subgraph, all entity object types will be "automatically" reachable (from the root query
    // type) because of the `_entities` field (it returns `_Entity`, which is a union of all entity
    // object types, making those reachable.
    //
    // However, we also want entity interface types (interfaces with an @key) to be reachable in a
    // similar way, because the `_entities` field is also technically the one resolving them, and
    // not having them reachable would break plenty of code that assume that by traversing a query
    // graph from root, we get to everything that can be queried.
    //
    // But because GraphQL unions cannot have interface types, they are not part of the `_Entity`
    // union (and cannot be). This is ok as far as the typing of the schema goes, because even when
    // `_entities` is called to resolve an interface type, it technically returns a concrete object,
    // and so, since every implementation of an entity interface is also an entity, this is captured
    // by the `_Entity` union.
    //
    // But it does mean we want to manually add the corresponding edges now for interfaces, or
    // @key on interfaces wouldn't work properly (at least, when the interface is not otherwise
    // reachable by an operation on the subgraph).
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
                source: self.base.source.clone(),
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

fn resolvable_key_applications(
    directives: &[Component<Directive>],
    key_directive_definition_name: &Name,
    federation_spec_definition: &'static FederationSpecDefinition,
) -> Result<Vec<KeyDirectiveArguments>, FederationError> {
    let mut applications = Vec::new();
    for directive in directives {
        if directive.name != *key_directive_definition_name {
            continue;
        }
        let key_directive_application =
            federation_spec_definition.key_directive_arguments(directive)?;
        if !key_directive_application.resolvable {
            continue;
        }
        applications.push(key_directive_application);
    }
    Ok(applications)
}
