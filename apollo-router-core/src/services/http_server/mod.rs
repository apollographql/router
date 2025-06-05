use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use tower::BoxError;
use tower::util::BoxCloneService;

type HttpServerService = BoxCloneService<
    http::Request<BoxBody<Bytes, BoxError>>,
    http::Response<BoxBody<Bytes, BoxError>>,
    BoxError,
>;

// Context is stored in extensions
