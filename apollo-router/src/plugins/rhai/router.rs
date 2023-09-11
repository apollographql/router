//! router module

use std::ops::ControlFlow;

use tower::BoxError;

use super::ErrorDetails;
use crate::graphql::Error;
pub(crate) use crate::services::router::*;
use crate::Context;

pub(crate) type FirstRequest = super::engine::RhaiRouterFirstRequest;
pub(crate) type ChunkedRequest = super::engine::RhaiRouterChunkedRequest;
pub(crate) type FirstResponse = super::engine::RhaiRouterResponse;
pub(crate) type DeferredResponse = super::engine::RhaiRouterChunkedResponse;

pub(super) fn request_failure(
    context: Context,
    error_details: ErrorDetails,
) -> Result<ControlFlow<crate::services::router::Response, Request>, BoxError> {
    let res = if let Some(body) = error_details.body {
        crate::services::router::Response::builder()
            .extensions(body.extensions)
            .errors(body.errors)
            .status_code(error_details.status)
            .context(context)
            .and_data(body.data)
            .and_label(body.label)
            .and_path(body.path)
            .build()?
    } else {
        crate::services::router::Response::error_builder()
            .errors(vec![Error {
                message: error_details.message.unwrap_or_default(),
                ..Default::default()
            }])
            .context(context)
            .status_code(error_details.status)
            .build()?
    };

    Ok(ControlFlow::Break(res))
}

pub(super) fn response_failure(
    context: Context,
    error_details: ErrorDetails,
) -> crate::services::router::Response {
    if let Some(body) = error_details.body {
        crate::services::router::Response::builder()
            .extensions(body.extensions)
            .errors(body.errors)
            .status_code(error_details.status)
            .context(context)
            .and_data(body.data)
            .and_label(body.label)
            .and_path(body.path)
            .build()
    } else {
        crate::services::router::Response::error_builder()
            .errors(vec![Error {
                message: error_details.message.unwrap_or_default(),
                ..Default::default()
            }])
            .status_code(error_details.status)
            .context(context)
            .build()
    }
    .expect("can't fail to build our error message")
}
