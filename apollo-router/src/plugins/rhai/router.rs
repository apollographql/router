//! router module

pub(crate) use crate::services::router::*;
pub(crate) type FirstRequest = super::engine::RhaiRouterFirstRequest;
pub(crate) type ChunkedRequest = super::engine::RhaiRouterChunkedRequest;
pub(crate) type Response = super::engine::RhaiRouterResponse;
pub(crate) type DeferredResponse = super::engine::RhaiRouterChunkedResponse;
