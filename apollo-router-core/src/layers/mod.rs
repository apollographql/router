use tower::ServiceBuilder;
use tower::layer::util::Stack;

pub mod http_to_bytes;

pub trait ServiceBuilderExt<L> {
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>>;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>> {
        self.layer(http_to_bytes::HttpToBytesLayer)
    }
}
