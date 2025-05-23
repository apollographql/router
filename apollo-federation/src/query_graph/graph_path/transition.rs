use petgraph::graph::EdgeIndex;

use crate::operation::Field;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::graph_path::GraphPath;
use crate::query_graph::graph_path::GraphPathTriggerVariant;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// A `GraphPath` whose triggers are query graph transitions in some other query graph (essentially
/// meaning that the path has been guided by a walk through that other query graph).
#[allow(dead_code)]
pub(crate) type TransitionGraphPath = GraphPath<QueryGraphEdgeTransition, EdgeIndex>;

impl GraphPathTriggerVariant for QueryGraphEdgeTransition {
    fn get_field_parent_type(&self) -> Option<CompositeTypeDefinitionPosition> {
        match self {
            QueryGraphEdgeTransition::FieldCollection {
                field_definition_position,
                ..
            } => Some(field_definition_position.parent()),
            _ => None,
        }
    }

    fn get_field_mut(&mut self) -> Option<&mut Field> {
        None
    }
}
