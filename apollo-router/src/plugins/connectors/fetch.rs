use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::name;
use apollo_federation::schema::ObjectFieldDefinitionPosition;
use apollo_federation::schema::ObjectOrInterfaceFieldDefinitionPosition;
use apollo_federation::schema::ObjectOrInterfaceFieldDirectivePosition;
use apollo_federation::sources::connect::ConnectId;
use apollo_federation::sources::connect::JSONSelection;
use apollo_federation::sources::to_remove;

use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Protocol;
use crate::query_planner::fetch::RestFetchNode;

impl From<FetchNode> for to_remove::FetchNode {
    fn from(value: FetchNode) -> to_remove::FetchNode {
        let subgraph_name = match value.protocol.as_ref() {
            Protocol::RestFetch(rf) => rf.parent_service_name.clone().into(),
            _ => value.service_name.clone(),
        };
        to_remove::FetchNode::Connect(to_remove::connect::FetchNode {
            source_id: ConnectId {
                label: value.service_name.to_string(),
                subgraph_name,
                directive: ObjectOrInterfaceFieldDirectivePosition {
                    field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                        ObjectFieldDefinitionPosition {
                            type_name: name!("TypeName"),
                            field_name: name!("field_name"),
                        },
                    ),
                    directive_name: name!("Directive__name"),
                    directive_index: 0,
                },
            },
            field_response_name: name!("Field"),
            field_arguments: Default::default(),
            selection: JSONSelection::empty(),
        })
    }
}

impl FetchNode {
    // TODO: let's go all in on nodestr
    pub(crate) fn update_connector_plan(
        &mut self,
        parent_service_name: &String,
        connectors: &Arc<HashMap<Arc<String>, super::Connector>>,
    ) {
        let parent_service_name = parent_service_name.to_string();
        let connector = connectors.get(&self.service_name.to_string()).unwrap();
        let service_name =
            std::mem::replace(&mut self.service_name, connector.display_name().into());
        self.protocol = Arc::new(Protocol::RestFetch(RestFetchNode {
            connector_service_name: service_name.to_string(),
            connector_graph_key: connector._name(),
            parent_service_name,
        }));
        let as_fednext_node: to_remove::FetchNode = self.clone().into();
        self.source_node = Some(Arc::new(as_fednext_node));
    }
}
