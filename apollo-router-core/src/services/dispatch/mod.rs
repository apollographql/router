use crate::Extensions;
use crate::json::JsonValue;
use futures::{Stream, TryFutureExt};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{BoxError, Service};

pub struct Request<T> {
    pub extensions: Extensions,
    // Services are cached by name in the FetchService.
    pub service_name: String,
    // This is opaque data identified by type ID when constructing the downstream service
    pub body: T,

    pub variables: HashMap<String, JsonValue>,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("extensions", &self.extensions)
            .field("responses", &"<stream>")
            .finish()
    }
}

/// Error types for request dispatcher
#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// No handler registered for request type
    #[error("No handler registered for request type: {type_name}")]
    #[diagnostic(
        code(APOLLO_ROUTER_REQUEST_DISPATCHER_NO_HANDLER),
        help("Ensure a handler is registered for this request type")
    )]
    NoHandlerForType {
        #[extension("typeName")]
        type_name: String,
        #[extension("typeId")]
        type_id: String,
    },


    /// Request downcasting failed
    #[error("Failed to downcast request body")]
    #[diagnostic(
        code(APOLLO_ROUTER_REQUEST_DISPATCHER_DOWNCAST_FAILED),
        help("Ensure the request body type matches the expected type")
    )]
    DowncastFailed {
        #[extension("expectedType")]
        expected_type: String,
        #[extension("actualTypeId")]
        actual_type_id: String,
    },
}

/// Trait for cloneable services that handle Any requests
pub trait CloneableService:
    Service<
        Request<Box<dyn Any + Send + 'static>>,
        Response = Response,
        Error = BoxError,
        Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send>>,
    > + Send
{
    fn clone_service(&self) -> Box<dyn CloneableService>;
}

/// Type-erased service that can handle Any requests
type AnyService = Box<dyn CloneableService>;

/// Request dispatcher that routes requests based on their body type
///
/// # Backpressure and Load Shedding
///
/// This service preserves backpressure by driving all downstream handlers to readiness
/// in `poll_ready` before indicating readiness. However, a single handler that is not
/// ready will mark the entire dispatcher as not ready.
///
/// For optimal load management, downstream handlers should implement their own load
/// shedding mechanisms to prevent a single slow handler from blocking all requests:
/// - Circuit breakers
/// - Rate limiting
/// - Queue depth monitoring
/// - Resource-based admission control
///
/// The dispatcher clones and swaps handlers in `call` to avoid the need for oneshot
/// channels while maintaining proper backpressure propagation.
pub struct RequestDispatcher {
    handlers: HashMap<TypeId, AnyService>,
}

impl Clone for RequestDispatcher {
    fn clone(&self) -> Self {
        let cloned_handlers = self
            .handlers
            .iter()
            .map(|(k, v)| (*k, v.clone_service()))
            .collect();
        Self {
            handlers: cloned_handlers,
        }
    }
}

impl RequestDispatcher {
    /// Create a new RequestDispatcher with the given handlers
    pub fn new(handlers: HashMap<TypeId, AnyService>) -> Self {
        Self { handlers }
    }
}

impl Service<Request<Box<dyn Any + Send + 'static>>> for RequestDispatcher {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Poll all handlers for readiness
        let mut all_ready = true;

        for handler in self.handlers.values_mut() {
            match handler.poll_ready(cx) {
                Poll::Ready(Ok(())) => continue,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {
                    all_ready = false;
                    // Continue polling others to ensure they're all woken
                }
            }
        }

        if all_ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn call(&mut self, req: Request<Box<dyn Any + Send + 'static>>) -> Self::Future {
        let type_id = (*req.body).type_id();

        // Clone the handler and swap it with the original
        let handler = match self.handlers.get_mut(&type_id) {
            Some(handler) => {
                let cloned = handler.clone_service();
                std::mem::replace(handler, cloned)
            }
            None => {
                let type_name = std::any::type_name_of_val(&*req.body).to_string();
                let type_id_str = format!("{:?}", type_id);
                return Box::pin(async move {
                    Err(Error::NoHandlerForType {
                        type_name,
                        type_id: type_id_str,
                    }
                    .into())
                });
            }
        };

        Box::pin(async move {
            // The handler should already be ready since we polled it in poll_ready
            let mut handler = handler;
            handler.call(req).await.map_err(Into::into)
        })
    }
}

/// Trait for registering typed handlers and creating RequestDispatcher
pub trait HandlerRegistration {
    /// Register a handler for a specific request body type
    fn register_handler<T, S>(&mut self, handler: S) -> &mut Self
    where
        T: Any + Send + Sync + 'static,
        S: Service<Request<T>, Response = Response, Error = BoxError>
            + Send
            + Sync
            + 'static
            + Clone,
        S::Future: Send + 'static;

    /// Create a RequestDispatcher with the registered handlers
    fn into_dispatcher(self) -> RequestDispatcher;
}

impl HandlerRegistration for HashMap<TypeId, AnyService> {
    fn register_handler<T, S>(&mut self, handler: S) -> &mut Self
    where
        T: Any + Send + Sync + 'static,
        S: Service<Request<T>, Response = Response> + Send + Sync + 'static + Clone,
        S::Future: Send + 'static,
        S::Error: Into<BoxError> + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();

        // Create a typed handler wrapper
        let wrapper = TypedHandlerWrapper {
            inner: handler,
            _phantom: std::marker::PhantomData,
        };

        self.insert(type_id, Box::new(wrapper));
        self
    }

    fn into_dispatcher(self) -> RequestDispatcher {
        RequestDispatcher::new(self)
    }
}

/// Convenient builder type for creating RequestDispatcher with registered handlers
pub type RequestDispatcherBuilder = HashMap<TypeId, AnyService>;

impl RequestDispatcher {
    /// Create a new builder for RequestDispatcher
    pub fn builder() -> RequestDispatcherBuilder {
        HashMap::<TypeId, AnyService>::new()
    }

    /// Create a RequestDispatcher from a collection that implements HandlerRegistration
    pub fn from_registration<R: HandlerRegistration>(registry: R) -> RequestDispatcher {
        registry.into_dispatcher()
    }
}

/// Wrapper that adapts a typed handler to work with Any requests
#[derive(Clone)]
struct TypedHandlerWrapper<T, S> {
    inner: S,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, S> Service<Request<Box<dyn Any + Send + 'static>>> for TypedHandlerWrapper<T, S>
where
    T: Any + Send + Sync + 'static,
    S: Service<Request<T>, Response = Response> + Clone + Send,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + Sync + 'static,
{
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<Box<dyn Any + Send + 'static>>) -> Self::Future {
        let Request {
            extensions,
            service_name,
            body,
            variables,
        } = req;

        // Try to downcast the body
        let body = match body.downcast::<T>() {
            Ok(typed_body) => *typed_body,
            Err(any_body) => {
                let actual_type_id = (*any_body).type_id();
                return Box::pin(async move {
                    Err(Error::DowncastFailed {
                        expected_type: std::any::type_name::<T>().to_string(),
                        actual_type_id: format!("{:?}", actual_type_id),
                    }
                    .into())
                });
            }
        };

        // Create typed request
        let typed_req = Request {
            extensions,
            service_name,
            body,
            variables,
        };

        // Call the inner handler
        let fut = self.inner.call(typed_req).map_err(Into::into);
        Box::pin(fut)
    }
}

impl<T, S> CloneableService for TypedHandlerWrapper<T, S>
where
    T: Any + Send + Sync + 'static,
    S: Service<Request<T>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + Sync + 'static,
{
    fn clone_service(&self) -> Box<dyn CloneableService> {
        Box::new(TypedHandlerWrapper {
            inner: self.inner.clone(),
            _phantom: self._phantom,
        })
    }
}

#[cfg(test)]
mod tests;
