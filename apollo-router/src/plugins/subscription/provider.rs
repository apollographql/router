use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use once_cell::sync::Lazy;
use tower::BoxError;
use tower::util::BoxService;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::plugins::subscription::notification::Notify;
use crate::graphql;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

pub trait SubscriptionProvider: Send + Sync {
    fn create_service(
        &self,
        inner: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
        notify: Notify<String, graphql::Response>,
        service_name: String,
        config: serde_json::Value,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError>;
}

static REGISTRY: Lazy<RwLock<HashMap<String, Arc<dyn SubscriptionProvider>>>> = Lazy::new(|| RwLock::new(HashMap::new()));

pub fn register_provider(name: impl Into<String>, provider: impl SubscriptionProvider + 'static) {
    let mut registry = REGISTRY.write().expect("Registry lock poisoned");
    registry.insert(name.into(), Arc::new(provider));
}

pub fn get_provider(name: &str) -> Option<Arc<dyn SubscriptionProvider>> {
    let registry = REGISTRY.read().expect("Registry lock poisoned");
    registry.get(name).cloned()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomMode {
    pub provider_name: String,
    #[serde(default)]
    pub subgraphs: HashSet<String>,
    #[serde(default)]
    pub config: serde_json::Value,
}

