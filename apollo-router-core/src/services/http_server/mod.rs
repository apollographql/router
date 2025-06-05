use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use tower::BoxError;

type Request = http::Request<UnsyncBoxBody<Bytes, BoxError>>;
type Response = http::Response<UnsyncBoxBody<Bytes, BoxError>>;

// Context is stored in extensions
