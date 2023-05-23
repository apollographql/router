// With regards to ELv2 licensing, this entire file is license key functionality

//! Create a new tower Service instance.
use tower::Service;

/// Trait
pub(crate) trait ServiceFactory<Request> {
    type Service: Service<Request>;

    fn create(&self) -> Self::Service;
}
