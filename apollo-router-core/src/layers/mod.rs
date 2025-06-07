use tower::ServiceBuilder;
use tower::layer::util::Stack;

pub mod bytes_to_http;
pub mod bytes_to_json;
pub mod http_to_bytes;
pub mod json_to_bytes;

pub use bytes_to_http::Error as BytesToHttpError;
pub use bytes_to_json::Error as BytesToJsonError;
pub use http_to_bytes::Error as HttpToBytesError;
pub use json_to_bytes::Error as JsonToBytesError;

pub trait ServiceBuilderExt<L> {
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>>;
    fn bytes_to_json(self) -> ServiceBuilder<Stack<bytes_to_json::BytesToJsonLayer, L>>;
    fn json_to_bytes(self) -> ServiceBuilder<Stack<json_to_bytes::JsonToBytesLayer, L>>;
    fn bytes_to_http(self) -> ServiceBuilder<Stack<bytes_to_http::BytesToHttpLayer, L>>;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>> {
        self.layer(http_to_bytes::HttpToBytesLayer)
    }

    fn bytes_to_json(self) -> ServiceBuilder<Stack<bytes_to_json::BytesToJsonLayer, L>> {
        self.layer(bytes_to_json::BytesToJsonLayer)
    }

    fn json_to_bytes(self) -> ServiceBuilder<Stack<json_to_bytes::JsonToBytesLayer, L>> {
        self.layer(json_to_bytes::JsonToBytesLayer)
    }

    fn bytes_to_http(self) -> ServiceBuilder<Stack<bytes_to_http::BytesToHttpLayer, L>> {
        self.layer(bytes_to_http::BytesToHttpLayer::new())
    }
}
