//! subgraph module

use std::ops::ControlFlow;

use tower::BoxError;

use super::ErrorDetails;
use crate::Context;
use crate::graphql::Error;
pub(crate) use crate::services::subgraph::*;

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
            .subgraph_name(String::default()) // XXX: We don't know the subgraph name
            .build()
    } else {
        Response::error_builder()
            .errors(vec![
                Error::builder()
                    .message(error_details.message.unwrap_or_default())
                    .build(),
            ])
            .context(context)
            .status_code(error_details.status)
            .subgraph_name(String::default()) // XXX: We don't know the subgraph name
            .build()
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
            .subgraph_name(String::default()) // XXX: We don't know the subgraph name
            .build()
    } else {
        Response::error_builder()
            .errors(vec![
                Error::builder()
                    .message(error_details.message.unwrap_or_default())
                    .build(),
            ])
            .status_code(error_details.status)
            .context(context)
            .subgraph_name(String::default()) // XXX: We don't know the subgraph name
            .build()
    }
}
