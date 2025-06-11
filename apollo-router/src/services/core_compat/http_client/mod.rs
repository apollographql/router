//! HTTP Client compatibility layer between apollo-router and apollo-router-core
//!
//! This module provides conversion functions between the apollo-router HTTP types
//! and apollo-router-core HTTP types to enable gradual migration to the core types.
//!
//! ## Key Conversions
//!
//! - **Router to Core**: Converts router Context to core Extensions, stores additional fields in Extensions
//! - **Core to Router**: Extracts stored fields from Extensions and reconstructs Context
//! - **Body Conversion**: Handles conversion between RouterBody and UnsyncBoxBody types
//!
//! ## Usage
//!
//! ```rust,ignore
//! use apollo_router::services::core_compat::http_client::*;
//! 
//! // Convert router request to core request
//! let core_request = router_to_core_http_request(router_request)?;
//! 
//! // Convert core response back to router response  
//! let router_response = core_to_router_http_response(core_response, original_context)?;
//! ```

use apollo_router_core::{Extensions, services::http_client};
use http_body_util::{BodyExt, combinators::UnsyncBoxBody};
use tower::BoxError;

use crate::Context;
use crate::services::http::{HttpRequest as RouterHttpRequest, HttpResponse as RouterHttpResponse};

/// Key used to store the original router Context in core Extensions
const ROUTER_CONTEXT_KEY: &str = "apollo::core_compat::router_context";

/// Error types for HTTP client compatibility conversions
#[derive(Debug, thiserror::Error)]
pub enum ConversionError {
    /// Failed to convert HTTP request body
    #[error("Failed to convert HTTP request body: {source}")]
    RequestBodyConversion {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Failed to convert HTTP response body
    #[error("Failed to convert HTTP response body: {source}")]
    ResponseBodyConversion {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Failed to extract router context from core extensions
    #[error("Failed to extract router context from core extensions")]
    ContextExtractionFailed,
}

/// Convert a router HTTP request to a core HTTP request
///
/// This function:
/// 1. Stores the router Context in core Extensions 
/// 2. Converts RouterBody to UnsyncBoxBody<Bytes, BoxError>
/// 3. Preserves all HTTP headers and metadata
///
/// # Arguments
/// * `request` - The router HTTP request to convert
///
/// # Returns
/// * `Result<http_client::Request, ConversionError>` - The converted core HTTP request
pub async fn router_to_core_http_request(
    request: RouterHttpRequest,
) -> Result<http_client::Request, ConversionError> {
    let RouterHttpRequest { http_request, context } = request;
    
    let (parts, body) = http_request.into_parts();
    
    // Convert RouterBody to UnsyncBoxBody<Bytes, BoxError>
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ConversionError::RequestBodyConversion {
            source: Box::new(e),
        })?
        .to_bytes();
    
    let core_body = UnsyncBoxBody::new(
        http_body_util::Full::new(body_bytes)
            .map_err(|e: std::convert::Infallible| -> BoxError { match e {} })
    );
    
    // Create core Extensions and store the router Context
    let mut core_extensions = Extensions::new();
    core_extensions.insert(context);
    
    // Create the core HTTP request
    let mut core_request = http::Request::from_parts(parts, core_body);
    
    // Store core Extensions in http::Extensions for the core service
    let http_extensions: http::Extensions = core_extensions.into();
    *core_request.extensions_mut() = http_extensions;
    
    Ok(core_request)
}

/// Convert a core HTTP response back to a router HTTP response
///
/// This function:
/// 1. Extracts the router Context from core Extensions
/// 2. Converts UnsyncBoxBody back to RouterBody  
/// 3. Preserves all HTTP headers and status
///
/// # Arguments
/// * `response` - The core HTTP response to convert
///
/// # Returns
/// * `Result<RouterHttpResponse, ConversionError>` - The converted router HTTP response
pub async fn core_to_router_http_response(
    response: http_client::Response,
) -> Result<RouterHttpResponse, ConversionError> {
    let (mut parts, body) = response.into_parts();
    
    // Extract core Extensions from http::Extensions
    let core_extensions: Extensions = std::mem::take(&mut parts.extensions).into();
    
    // Extract the router Context from core Extensions
    let context = core_extensions
        .get::<Context>()
        .ok_or(ConversionError::ContextExtractionFailed)?;
    
    // Convert UnsyncBoxBody back to RouterBody
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ConversionError::ResponseBodyConversion {
            source: e.into(),
        })?
        .to_bytes();
    
    let router_body = crate::services::router::body::from_bytes(body_bytes);
    
    // Create the router HTTP response
    let http_response = http::Response::from_parts(parts, router_body);
    
    Ok(RouterHttpResponse {
        http_response,
        context,
    })
}

/// Convert a core HTTP request to a router HTTP request
///
/// This is the inverse of `router_to_core_http_request` and is used when
/// a core service needs to call into a router service.
///
/// # Arguments
/// * `request` - The core HTTP request to convert
///
/// # Returns
/// * `Result<RouterHttpRequest, ConversionError>` - The converted router HTTP request
pub async fn core_to_router_http_request(
    request: http_client::Request,
) -> Result<RouterHttpRequest, ConversionError> {
    let (mut parts, body) = request.into_parts();
    
    // Extract core Extensions from http::Extensions
    let core_extensions: Extensions = std::mem::take(&mut parts.extensions).into();
    
    // Extract the router Context from core Extensions, or create a default one
    let context = core_extensions
        .get::<Context>()
        .unwrap_or_else(Context::default);
    
    // Convert UnsyncBoxBody to RouterBody
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ConversionError::RequestBodyConversion {
            source: e.into(),
        })?
        .to_bytes();
    
    let router_body = crate::services::router::body::from_bytes(body_bytes);
    
    // Create the router HTTP request
    let http_request = http::Request::from_parts(parts, router_body);
    
    Ok(RouterHttpRequest {
        http_request,
        context,
    })
}

/// Convert a router HTTP response to a core HTTP response
///
/// This is the inverse of `core_to_router_http_response` and is used when
/// a router service needs to return a response that core services can consume.
///
/// # Arguments  
/// * `response` - The router HTTP response to convert
///
/// # Returns
/// * `Result<http_client::Response, ConversionError>` - The converted core HTTP response
pub async fn router_to_core_http_response(
    response: RouterHttpResponse,
) -> Result<http_client::Response, ConversionError> {
    let RouterHttpResponse { http_response, context } = response;
    
    let (parts, body) = http_response.into_parts();
    
    // Convert RouterBody to UnsyncBoxBody<Bytes, BoxError>
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ConversionError::ResponseBodyConversion {
            source: Box::new(e),
        })?
        .to_bytes();
    
    let core_body = UnsyncBoxBody::new(
        http_body_util::Full::new(body_bytes)
            .map_err(|e: std::convert::Infallible| -> BoxError { match e {} })
    );
    
    // Create core Extensions and store the router Context
    let mut core_extensions = Extensions::new();
    core_extensions.insert(context);
    
    // Create the core HTTP response
    let mut core_response = http::Response::from_parts(parts, core_body);
    
    // Store core Extensions in http::Extensions for core services
    let http_extensions: http::Extensions = core_extensions.into();
    *core_response.extensions_mut() = http_extensions;
    
    Ok(core_response)
}

#[cfg(test)]
mod tests; 