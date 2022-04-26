//! Utilities which make it easy to test with [`crate::plugin`].

pub mod mock;
pub mod service;

pub use service::{
    MockExecutionService, MockQueryPlanningService, MockRouterService, MockSubgraphService,
};
