//! execution module

pub(crate) use crate::services::execution::*;
pub(crate) type FirstResponse = super::engine::RhaiExecutionResponse;
pub(crate) type DeferredResponse = super::engine::RhaiExecutionDeferredResponse;
