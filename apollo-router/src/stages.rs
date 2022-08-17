//! Types for Tower services at the different stages of handling a request.

pub mod http {
    use tower::BoxError;

    pub type Request = crate::http_ext::Request<hyper::Body>;
    pub type Response = crate::http_ext::Response<hyper::Body>;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
    pub type Result = std::result::Result<Response, BoxError>;
}

pub mod supergraph {
    use tower::BoxError;

    pub use crate::services::SupergraphRequest as Request;
    pub use crate::services::SupergraphResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
    pub type Result = std::result::Result<Response, BoxError>;
}

pub mod query_planner {
    use tower::BoxError;

    pub use crate::services::QueryPlannerRequest as Request;
    pub use crate::services::QueryPlannerResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
    pub type Result = std::result::Result<Response, BoxError>;

    // Reachable from Request or Response:
    pub use crate::query_planner::QueryPlan;
    pub use crate::services::QueryPlannerContent;
    pub use crate::spec::Query;
}

pub mod execution {
    use tower::BoxError;

    pub use crate::services::ExecutionRequest as Request;
    pub use crate::services::ExecutionResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
    pub type Result = std::result::Result<Response, BoxError>;
}

pub mod subgraph {
    use tower::BoxError;

    pub use crate::services::SubgraphRequest as Request;
    pub use crate::services::SubgraphResponse as Response;
    pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
    pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
    pub type Result = std::result::Result<Response, BoxError>;

    // Reachable from Request or Response:
    pub use crate::query_planner::OperationKind;
}
