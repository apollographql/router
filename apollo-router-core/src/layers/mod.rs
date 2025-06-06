use crate::services;
use tower::layer::util::Stack;
use tower::{Service, ServiceBuilder};

pub mod bytes_to_json;
pub mod http_to_bytes;

pub use bytes_to_json::Error as BytesToJsonError;
pub use http_to_bytes::Error as HttpToBytesError;

pub trait ServiceBuilderExt<L> {
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>>;
    fn bytes_to_json(self) -> ServiceBuilder<Stack<bytes_to_json::BytesToJsonLayer, L>>;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>> {
        self.layer(http_to_bytes::HttpToBytesLayer)
    }

    fn bytes_to_json(self) -> ServiceBuilder<Stack<bytes_to_json::BytesToJsonLayer, L>> {
        self.layer(bytes_to_json::BytesToJsonLayer)
    }
}
