use std::collections::HashMap;

use apollo_compiler::name;
use apollo_compiler::NodeStr;
use indexmap::IndexSet;
use itertools::Either;
use itertools::Itertools;
use petgraph::prelude::NodeIndex;
use strum::IntoEnumIterator;

use crate::error::FederationError;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::SchemaRootDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphSubBuilderApi;
use crate::sources::graphql::federated_query_graph::TypeConditionEdge;
use crate::sources::graphql::GraphqlId;
use crate::sources::source;
use crate::sources::source::federated_query_graph::builder::FederatedQueryGraphBuilderApi;
use crate::sources::source::federated_query_graph::EnumNode;
use crate::sources::source::federated_query_graph::ScalarNode;
use crate::ValidFederationSubgraph;

use super::ConcreteFieldEdge;

#[derive(Default)]
pub(crate) struct FederatedQueryGraphBuilder;

impl FederatedQueryGraphBuilderApi for FederatedQueryGraphBuilder {
    fn process_subgraph_schema(
        &self,
        subgraph: ValidFederationSubgraph,
        builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError> {
        let src_id = GraphqlId::from(NodeStr::new(&subgraph.name));
        let schema = &subgraph.schema;
        let source::federated_query_graph::FederatedQueryGraph::Graphql(graph) =
            builder.source_query_graph()?
        else {
            return Err(FederationError::internal("While processing GraphQL subgraph schema, received a non-GraphQL source query graph."));
        };
        graph
            .subgraphs_by_source
            .insert(src_id.clone(), subgraph.clone());
        let mut existing_types: HashMap<OutputTypeDefinitionPosition, NodeIndex> = HashMap::new();
        let mut sub_builder = builder.add_source(src_id.into())?;
        for root_kind in SchemaRootDefinitionKind::iter() {
            let position = SchemaRootDefinitionPosition { root_kind };
            let Some(position) = position.try_get(schema.schema()) else {
                continue;
            };
            let ty: OutputTypeDefinitionPosition =
                schema.get_type(position.name.clone())?.try_into()?;
            if is_source_entering_type_ignored(&ty, schema)? {
                continue;
            }
            process_type(ty, schema, &mut sub_builder, &mut existing_types)?;
        }
        let Some(metadata) = schema.subgraph_metadata() else {
            return Err(FederationError::internal(
                "Expected subgraph metadata while processing GraphQL subgraph but found none.",
            ));
        };
        let key_name = metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?;
        schema
            .referencers()
            .get_directive(&key_name.name)?
            .object_types
            .iter()
            .map(|ty| OutputTypeDefinitionPosition::from(ty.clone()))
            .try_for_each(|ty| {
                if is_source_entering_type_ignored(&ty, schema)? {
                    return Ok(());
                }
                process_type(ty, schema, &mut sub_builder, &mut existing_types).map(drop)
            })
    }
}

fn process_type(
    output_type_definition_position: OutputTypeDefinitionPosition,
    subgraph_schema: &ValidFederationSchema,
    builder: &mut impl IntraSourceQueryGraphSubBuilderApi,
    existing_types: &mut HashMap<OutputTypeDefinitionPosition, NodeIndex>,
) -> Result<NodeIndex, FederationError> {
    if let Some(index) = existing_types.get(&output_type_definition_position) {
        return Ok(*index);
    }
    let type_name = output_type_definition_position.type_name().clone();
    let (ty, index) = match output_type_definition_position.clone() {
        OutputTypeDefinitionPosition::Scalar(ty) => (
            None,
            builder.add_scalar_node(type_name, ScalarNode::Graphql(ty.into()))?,
        ),
        OutputTypeDefinitionPosition::Enum(ty) => (
            None,
            builder.add_enum_node(type_name, EnumNode::Graphql(ty.into()))?,
        ),
        OutputTypeDefinitionPosition::Object(subgraph_type) => {
            let ty = Some(Either::Left(subgraph_type.clone()));
            let node = super::ConcreteNode {
                subgraph_type,
                provides_directive: None,
            };
            (ty, builder.add_concrete_node(type_name, node.into())?)
        }
        OutputTypeDefinitionPosition::Interface(ty) => {
            let subgraph_type = AbstractTypeDefinitionPosition::from(ty);
            let ty = Some(Either::Right(subgraph_type.clone()));
            let node = super::AbstractNode {
                subgraph_type,
                provides_directive: None,
            };
            (ty, builder.add_abstract_node(type_name, node.into())?)
        }
        OutputTypeDefinitionPosition::Union(ty) => {
            let subgraph_type = AbstractTypeDefinitionPosition::from(ty);
            let ty = Some(Either::Right(subgraph_type.clone()));
            let node = super::AbstractNode {
                subgraph_type,
                provides_directive: None,
            };
            (ty, builder.add_abstract_node(type_name, node.into())?)
        }
    };
    existing_types.insert(output_type_definition_position, index);
    match ty {
        Some(Either::Left(concrete_ty)) => concrete_ty
            .field_positions(subgraph_schema.schema())?
            .try_for_each(|subgraph_field| {
            if !is_field_ignored(subgraph_field.clone().into(), subgraph_schema)? {
                return Ok(());
            }
            subgraph_field
                .get(subgraph_schema.schema())
                .and_then(|field| subgraph_schema.get_type(field.ty.inner_named_type().clone()))
                .and_then(OutputTypeDefinitionPosition::try_from)
                .and_then(|ty| process_type(ty, subgraph_schema, builder, existing_types))
                .and_then(|next_index| {
                    builder
                        .add_concrete_field_edge(
                            index,
                            next_index,
                            subgraph_field.field_name.clone(),
                            IndexSet::new(),
                            ConcreteFieldEdge {
                                subgraph_field,
                                requires_condition: None,
                            }
                            .into(),
                        )
                        .map(drop)
                })
        })?,
        Some(Either::Right(abstract_ty)) => subgraph_schema
            .possible_runtime_types(abstract_ty.into())?
            .into_iter()
            .try_for_each(|ty| {
                process_type(ty.clone().into(), subgraph_schema, builder, existing_types).and_then(
                    |next_index| {
                        builder
                            .add_type_condition_edge(
                                index,
                                next_index,
                                TypeConditionEdge::new(ty.into()).into(),
                            )
                            .map(drop)
                    },
                )
            })?,
        None => {}
    }
    Ok(index)
}

fn is_field_ignored(
    field_definition_position: FieldDefinitionPosition,
    subgraph_schema: &ValidFederationSchema,
) -> Result<bool, FederationError> {
    if field_definition_position.field_name().starts_with("__") {
        return Ok(true);
    }

    if let FieldDefinitionPosition::Object(position) = &field_definition_position {
        let query_def = ObjectTypeDefinitionPosition {
            type_name: name!("Query"),
        };
        let entities = query_def.field(name!("_entities"));
        let service = query_def.field(name!("_service"));
        if [entities, service].contains(position) {
            return Ok(true);
        }
    }

    subgraph_schema
        .subgraph_metadata()
        .ok_or_else(|| FederationError::internal("Expected subgraph metadata while processing GraphQL subgraph schema but found none."))
        .and_then(|metadata| {
            metadata
                .external_metadata()
                .is_external(&field_definition_position)
        })
}

fn is_source_entering_type_ignored(
    output_type_definition_position: &OutputTypeDefinitionPosition,
    subgraph_schema: &ValidFederationSchema,
) -> Result<bool, FederationError> {
    let OutputTypeDefinitionPosition::Object(ty) = output_type_definition_position else {
        return Ok(false);
    };
    ty.field_positions(subgraph_schema.schema())?
        .map(|field| is_field_ignored(field.into(), subgraph_schema))
        .process_results(|mut results| results.all(|b| b))
}
