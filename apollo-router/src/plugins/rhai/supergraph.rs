//! supergraph module

use std::ops::ControlFlow;

use tower::BoxError;

use super::ErrorDetails;
use crate::graphql::Error;
pub(crate) use crate::services::supergraph::*;
use crate::Context;

pub(crate) type FirstResponse = super::engine::RhaiSupergraphResponse;
pub(crate) type DeferredResponse = super::engine::RhaiSupergraphDeferredResponse;

pub(super) fn request_failure(
    context: Context,
    error_details: ErrorDetails,
) -> Result<ControlFlow<Response, Request>, BoxError> {
    let res = if let Some(body) = error_details.body {
        Response::builder()
            .extensions(body.extensions)
            .errors(body.errors)
            .status_code(error_details.status)
            .context(context)
            .and_data(body.data)
            .and_label(body.label)
            .and_path(body.path)
            .build()?
    } else {
        Response::error_builder()
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

pub(super) fn response_failure(context: Context, error_details: ErrorDetails) -> Response {
    if let Some(body) = error_details.body {
        Response::builder()
            .extensions(body.extensions)
            .errors(body.errors)
            .status_code(error_details.status)
            .context(context)
            .and_data(body.data)
            .and_label(body.label)
            .and_path(body.path)
            .build()
    } else {
        Response::error_builder()
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
