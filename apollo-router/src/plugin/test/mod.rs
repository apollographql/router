//! Utilities which make it easy to test with [`crate::plugin`].

mod mock;
#[macro_use]
mod service;
mod broken;
mod restricted;

pub use mock::subgraph::MockSubgraph;
pub use service::MockExecutionService;
pub use service::MockHttpClientService;
#[cfg(test)]
pub(crate) use service::MockInternalHttpClientService;
pub use service::MockRouterService;
pub use service::MockSubgraphService;
pub use service::MockSupergraphService;

pub(crate) use self::mock::canned;
