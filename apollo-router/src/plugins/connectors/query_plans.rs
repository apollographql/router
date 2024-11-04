use std::sync::Arc;

use apollo_federation::sources::connect::Connector;
use indexmap::IndexMap;

use crate::query_planner::PlanNode;
use crate::Context;

type ConnectorsByServiceName = Arc<IndexMap<Arc<str>, Connector>>;

pub(crate) fn store_connectors(
    context: &Context,
    connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
) {
    context
        .extensions()
        .with_lock(|mut lock| lock.insert::<ConnectorsByServiceName>(connectors_by_service_name));
}

pub(crate) fn get_connectors(context: &Context) -> Option<ConnectorsByServiceName> {
    context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorsByServiceName>().cloned())
}

type ConnectorLabels = Arc<IndexMap<Arc<str>, String>>;

pub(crate) fn store_connectors_labels(
    context: &Context,
    labels_by_service_name: Arc<IndexMap<Arc<str>, String>>,
) {
    context
        .extensions()
        .with_lock(|mut lock| lock.insert::<ConnectorLabels>(labels_by_service_name));
}

pub(crate) fn replace_connector_service_names_text(
    text: Option<Arc<String>>,
    context: &Context,
) -> Option<Arc<String>> {
    let replacements = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorLabels>().cloned());
    if let Some(replacements) = replacements {
        text.as_ref().map(|text| {
            let mut text = text.to_string();
            for (service_name, label) in replacements.iter() {
                text = text.replace(&**service_name, label);
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
        .with_lock(|lock| lock.get::<ConnectorLabels>().cloned());

    return if let Some(replacements) = replacements {
        let mut plan = plan.clone();
        recurse(Arc::make_mut(&mut plan), &replacements);
        plan
    } else {
        plan
    };

    fn recurse(plan: &mut PlanNode, replacements: &IndexMap<Arc<str>, String>) {
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
                if let Some(service_name) = replacements.get(&node.service_name) {
                    node.service_name = service_name.clone().into();
                }
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
