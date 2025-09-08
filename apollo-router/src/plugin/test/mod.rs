//! Utilities which make it easy to test with [`crate::plugin`].

mod mock;
#[macro_use]
mod service;
mod broken;
mod restricted;

#[cfg(test)]
pub use mock::connector::MockConnector;
pub use mock::subgraph::MockSubgraph;
pub use service::MockConnectorRequestService;
pub use service::MockConnectorService;
pub use service::MockExecutionService;
pub use service::MockHttpClientService;
#[cfg(test)]
pub(crate) use service::MockInternalHttpClientService;
pub use service::MockRouterService;
pub use service::MockSubgraphService;
pub use service::MockSupergraphService;

pub(crate) use self::mock::canned;
