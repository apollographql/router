#[cfg(feature = "service_mock")]
pub mod service;
pub mod structures;

#[cfg(feature = "service_mock")]
pub use service::{
    MockExecutionService, MockQueryPlanningService, MockRouterService, MockSubgraphService,
};
pub use structures::{RouterRequestBuilder, RouterResponseBuilder};
