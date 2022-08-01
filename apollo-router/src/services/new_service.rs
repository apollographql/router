//! Create a new tower Service instance.
use tower::Service;

/// Trait
pub(crate) trait NewService<Request> {
    type Service: Service<Request>;

    fn new_service(&self) -> Self::Service;
}
