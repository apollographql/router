use crate::Extensions;
use crate::json::JsonValue;
use futures::{Stream, TryFutureExt};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
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

    /// Handler execution failed
    #[error("Handler execution failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_REQUEST_DISPATCHER_HANDLER_FAILED),
        help("Check the handler implementation for errors")
    )]
    HandlerFailed {
        #[source]
        source: BoxError,
        #[extension("handlerType")]
        handler_type: String,
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

/// Type-erased handler factory that can create handlers for Any requests
type AnyHandlerFactory = Arc<dyn HandlerFactory + Send + Sync>;

/// Factory trait for creating service instances
pub trait HandlerFactory {
    fn create_handler(
        &self,
    ) -> Box<
        dyn Service<
                Request<Box<dyn Any + Send + 'static>>,
                Response = Response,
                Error = BoxError,
                Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send>>,
            > + Send,
    >;
}

/// Request dispatcher that routes requests based on their body type
///
/// # Backpressure and Load Shedding
///
/// **Important**: This service does not preserve backpressure from downstream handlers.
/// Each request creates a new handler instance from the factory, which means backpressure
/// signals from individual handlers are not propagated back to callers.
///
/// For proper load management, handlers should implement their own load shedding mechanisms,
/// such as:
/// - Circuit breakers
/// - Rate limiting
/// - Queue depth monitoring
/// - Resource-based admission control
///
/// The dispatcher itself is always ready to accept requests (`poll_ready` always returns
/// `Ready(Ok(()))`), so upstream services cannot rely on backpressure signals to control
/// load.
#[derive(Clone)]
pub struct RequestDispatcher {
    handlers: Arc<HashMap<TypeId, AnyHandlerFactory>>,
}

impl RequestDispatcher {
    /// Create a new RequestDispatcher with the given handlers
    pub fn new(handlers: HashMap<TypeId, AnyHandlerFactory>) -> Self {
        Self {
            handlers: Arc::new(handlers),
        }
    }
}

impl Service<Request<Box<dyn Any + Send + 'static>>> for RequestDispatcher {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Always ready since we're just dispatching - backpressure is not preserved.
        // Individual handlers must implement their own load shedding mechanisms.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Box<dyn Any + Send + 'static>>) -> Self::Future {
        let type_id = (*req.body).type_id();

        // Find the handler factory for this type
        let factory = match self.handlers.get(&type_id) {
            Some(factory) => factory,
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

        // Create a new handler instance outside the async block
        let handler = factory.create_handler();

        Box::pin(async move {
            // Manually ready and call the handler
            let mut handler = handler;
            
            // Ensure the handler is ready before calling
            futures::future::poll_fn(|cx| handler.poll_ready(cx))
                .await
                .map_err(|e| {
                    Error::HandlerFailed {
                        source: e,
                        handler_type: "Handler".to_string(),
                    }
                })?;
            
            // Now call the handler
            handler.call(req).await.map_err(|e| {
                Error::HandlerFailed {
                    source: e,
                    handler_type: "Handler".to_string(),
                }
                .into()
            })
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

impl HandlerRegistration for HashMap<TypeId, AnyHandlerFactory> {
    fn register_handler<T, S>(&mut self, handler: S) -> &mut Self
    where
        T: Any + Send + Sync + 'static,
        S: Service<Request<T>, Response = Response> + Send + Sync + 'static + Clone,
        S::Future: Send + 'static,
        S::Error: Into<BoxError> + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();

        // Create a factory that creates wrapper instances
        let factory = Arc::new(TypedHandlerFactory::<T, S> {
            inner: Arc::new(Mutex::new(handler)),
            _phantom: std::marker::PhantomData,
        });

        self.insert(type_id, factory);
        self
    }

    fn into_dispatcher(self) -> RequestDispatcher {
        RequestDispatcher::new(self)
    }
}

/// Convenient builder type for creating RequestDispatcher with registered handlers
pub type RequestDispatcherBuilder = HashMap<TypeId, AnyHandlerFactory>;

impl RequestDispatcher {
    /// Create a new builder for RequestDispatcher
    pub fn builder() -> RequestDispatcherBuilder {
        HashMap::<TypeId, AnyHandlerFactory>::new()
    }

    /// Create a RequestDispatcher from a collection that implements HandlerRegistration
    pub fn from_registration<R: HandlerRegistration>(registry: R) -> RequestDispatcher {
        registry.into_dispatcher()
    }
}

/// Factory that creates typed handler wrappers
struct TypedHandlerFactory<T, S> {
    inner: Arc<Mutex<S>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, S> HandlerFactory for TypedHandlerFactory<T, S>
where
    T: Any + Send + Sync + 'static,
    S: Service<Request<T>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + Sync + 'static,
{
    fn create_handler(
        &self,
    ) -> Box<
        dyn Service<
                Request<Box<dyn Any + Send + 'static>>,
                Response = Response,
                Error = BoxError,
                Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send>>,
            > + Send,
    > {
        let inner = self.inner.lock().unwrap().clone();
        Box::new(TypedHandlerWrapper {
            inner,
            _phantom: self._phantom,
        })
    }
}

/// Wrapper that adapts a typed handler to work with Any requests
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

#[cfg(test)]
mod tests;
