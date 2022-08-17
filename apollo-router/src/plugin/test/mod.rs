//! Utilities which make it easy to test with [`crate::plugin`].

mod mock;
#[macro_use]
mod service;

use std::collections::HashMap;
use std::sync::Arc;

pub use mock::subgraph::MockSubgraph;
pub use service::MockExecutionService;
pub use service::MockQueryPlanningService;
pub use service::MockRouterService;
pub use service::MockSubgraphService;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;

pub(crate) use self::mock::canned;
use crate::services::subgraph_service::SubgraphServiceFactory;
use crate::services::MakeSubgraphService;
use crate::services::Plugins;
use crate::services::SubgraphRequest;

#[derive(Clone)]
pub(crate) struct MockSubgraphFactory {
    pub(crate) subgraphs: HashMap<String, Arc<dyn MakeSubgraphService>>,
    pub(crate) plugins: Arc<Plugins>,
}

impl SubgraphServiceFactory for MockSubgraphFactory {
    type SubgraphService = BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError>;

    type Future =
        <BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError> as Service<
            SubgraphRequest,
        >>::Future;

    fn new_service(&self, name: &str) -> Option<Self::SubgraphService> {
        self.subgraphs.get(name).map(|service| {
            self.plugins
                .iter()
                .rev()
                .fold(service.make(), |acc, (_, e)| e.subgraph_service(name, acc))
        })
    }
}
