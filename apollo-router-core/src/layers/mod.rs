use tower::ServiceBuilder;
use tower::layer::util::Stack;

pub mod bytes_client_to_json_client;
pub mod bytes_to_http;
pub mod bytes_to_json;
pub mod fetch_to_http_client;
pub mod http_client_to_bytes_client;
pub mod http_to_bytes;
pub mod json_to_bytes;
pub mod prepare_query;

pub use bytes_client_to_json_client::Error as BytesClientToJsonClientError;
pub use bytes_to_http::Error as BytesToHttpError;
pub use bytes_to_json::Error as BytesToJsonError;
pub use fetch_to_http_client::Error as FetchToHttpClientError;
pub use http_client_to_bytes_client::Error as HttpClientToBytesClientError;
pub use http_to_bytes::Error as HttpToBytesError;
pub use json_to_bytes::Error as JsonToBytesError;
pub use prepare_query::Error as PrepareQueryError;

pub trait ServiceBuilderExt<L> {
    // Server-side transformations (request pipeline)
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>>;
    fn bytes_to_json(self) -> ServiceBuilder<Stack<bytes_to_json::BytesToJsonLayer, L>>;
    fn prepare_query<P, Pl>(self, query_parse_service: P, query_plan_service: Pl) -> ServiceBuilder<Stack<prepare_query::PrepareQueryLayer<P, Pl>, L>>;
    
    // Client-side transformations (fetch pipeline)
    fn fetch_to_http_client(self) -> ServiceBuilder<Stack<fetch_to_http_client::FetchToHttpClientLayer, L>>;
    fn http_client_to_bytes_client(self) -> ServiceBuilder<Stack<http_client_to_bytes_client::HttpClientToBytesClientLayer, L>>;
    fn bytes_client_to_json_client(self) -> ServiceBuilder<Stack<bytes_client_to_json_client::BytesClientToJsonClientLayer, L>>;
    
    // Response transformations (reverse direction)
    fn json_to_bytes(self) -> ServiceBuilder<Stack<json_to_bytes::JsonToBytesLayer, L>>;
    fn bytes_to_http(self) -> ServiceBuilder<Stack<bytes_to_http::BytesToHttpLayer, L>>;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    // Server-side transformations (request pipeline)
    fn http_to_bytes(self) -> ServiceBuilder<Stack<http_to_bytes::HttpToBytesLayer, L>> {
        self.layer(http_to_bytes::HttpToBytesLayer)
    }

    fn bytes_to_json(self) -> ServiceBuilder<Stack<bytes_to_json::BytesToJsonLayer, L>> {
        self.layer(bytes_to_json::BytesToJsonLayer)
    }

    fn prepare_query<P, Pl>(self, query_parse_service: P, query_plan_service: Pl) -> ServiceBuilder<Stack<prepare_query::PrepareQueryLayer<P, Pl>, L>> {
        self.layer(prepare_query::PrepareQueryLayer::new(query_parse_service, query_plan_service))
    }

    // Client-side transformations (fetch pipeline)
    fn fetch_to_http_client(self) -> ServiceBuilder<Stack<fetch_to_http_client::FetchToHttpClientLayer, L>> {
        self.layer(fetch_to_http_client::FetchToHttpClientLayer)
    }

    fn http_client_to_bytes_client(self) -> ServiceBuilder<Stack<http_client_to_bytes_client::HttpClientToBytesClientLayer, L>> {
        self.layer(http_client_to_bytes_client::HttpClientToBytesClientLayer)
    }

    fn bytes_client_to_json_client(self) -> ServiceBuilder<Stack<bytes_client_to_json_client::BytesClientToJsonClientLayer, L>> {
        self.layer(bytes_client_to_json_client::BytesClientToJsonClientLayer)
    }

    // Response transformations (reverse direction)
    fn json_to_bytes(self) -> ServiceBuilder<Stack<json_to_bytes::JsonToBytesLayer, L>> {
        self.layer(json_to_bytes::JsonToBytesLayer)
    }

    fn bytes_to_http(self) -> ServiceBuilder<Stack<bytes_to_http::BytesToHttpLayer, L>> {
        self.layer(bytes_to_http::BytesToHttpLayer::new())
    }
}
