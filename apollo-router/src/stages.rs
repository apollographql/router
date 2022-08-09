//! The four stages of handling a GraphQL request

use tower::BoxError;

pub mod router {
    use super::*;
    pub use crate::services::RouterRequest as Request;
    pub use crate::services::RouterResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
}

pub mod query_planner {
    use super::*;
    pub use crate::services::QueryPlannerRequest as Request;
    pub use crate::services::QueryPlannerResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;

    // Reachable from Request or Response:
    pub use crate::query_planner::QueryPlan;
    pub use crate::services::QueryPlannerContent;
    pub use crate::spec::Query;
}

pub mod execution {
    use super::*;
    pub use crate::services::ExecutionRequest as Request;
    pub use crate::services::ExecutionResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
}

pub mod subgraph {
    use super::*;
    pub use crate::services::SubgraphRequest as Request;
    pub use crate::services::SubgraphResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;

    // Reachable from Request or Response:
    pub use crate::query_planner::OperationKind;
}
