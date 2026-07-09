//! Tower fetcher for subgraphs.
//!
//! This module is organized as:
//! - `types`: the `Request`/`Response` types that flow through the subgraph service pipeline.
//! - `service`: the [`tower::Service`] implementation, batching, and the service factory.
//! - `http`: low-level HTTP client utilities (TLS config, response parsing) used by `service`.

pub(crate) mod http;
pub(crate) mod service;
mod types;

pub use self::types::BoxCloneService;
pub(crate) use self::types::BoxGqlStream;
pub use self::types::Request;
pub use self::types::Response;
pub use self::types::ServiceResult;
pub(crate) use self::types::SubgraphRequestId;
