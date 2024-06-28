use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::NodeStr;
use apollo_federation::sources::connect::Connector;
use indexmap::IndexMap;

use crate::query_planner::PlanNode;
use crate::Context;

type ConnectorsContext = Arc<HashMap<String, String>>;

pub(crate) fn store_connectors_context(
    context: &Context,
    connectors_by_service_name: &IndexMap<NodeStr, Connector>,
) {
    let map = Arc::new(
        connectors_by_service_name
            .iter()
            .map(|(k, v)| (k.to_string(), v.id.label.clone()))
            .collect::<HashMap<_, _>>(),
    );
    context
        .extensions()
        .with_lock(|mut lock| lock.insert::<ConnectorsContext>(map));
}

pub(crate) fn replace_connector_service_names_text(
    text: Option<Arc<String>>,
    context: &Context,
) -> Option<Arc<String>> {
    let replacements = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorsContext>().cloned());
    if let Some(replacements) = replacements {
        text.as_ref().map(|text| {
            let mut text = text.to_string();
            for (service_name, label) in replacements.iter() {
                text = text.replace(service_name, label);
            }
            Arc::new(text)
        })
    } else {
        text
    }
}

pub(crate) fn replace_connector_service_names(
    plan: Arc<PlanNode>,
    context: &Context,
) -> Arc<PlanNode> {
    let replacements = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorsContext>().cloned());

    return if let Some(replacements) = replacements {
        let mut plan = plan.clone();
        recurse(Arc::make_mut(&mut plan), &replacements);
        plan
    } else {
        plan
    };

    fn recurse(plan: &mut PlanNode, replacements: &HashMap<String, String>) {
        match plan {
            PlanNode::Sequence { nodes } => {
                for node in nodes {
                    recurse(node, replacements);
                }
            }
            PlanNode::Parallel { nodes } => {
                for node in nodes {
                    recurse(node, replacements);
                }
            }
            PlanNode::Fetch(node) => {
                node.service_name = replacements
                    .get(&node.service_name.to_string())
                    .map(|v| v.clone().into())
                    .unwrap_or_else(|| node.service_name.clone());
            }
            PlanNode::Flatten(flatten) => {
                recurse(&mut flatten.node, replacements);
            }
            PlanNode::Defer { primary, deferred } => {
                if let Some(primary) = primary.node.as_mut() {
                    recurse(primary, replacements);
                }
                for deferred in deferred {
                    if let Some(node) = &mut deferred.node {
                        recurse(Arc::make_mut(node), replacements);
                    }
                }
            }
            PlanNode::Subscription { primary: _, rest } => {
                // ignoring subscriptions because connectors are not supported
                for node in rest {
                    recurse(node, replacements);
                }
            }
            PlanNode::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                if let Some(if_clause) = if_clause.as_mut() {
                    recurse(if_clause, replacements);
                }
                if let Some(else_clause) = else_clause.as_mut() {
                    recurse(else_clause, replacements);
                }
            }
        }
    }
}
