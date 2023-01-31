//! Utilities which make it easy to test with [`crate::plugin`].

mod mock;
#[macro_use]
mod service;

pub use mock::subgraph::MockSubgraph;
pub use service::MockExecutionService;
pub use service::MockSubgraphService;
pub use service::MockSupergraphService;

pub(crate) use self::mock::canned;
