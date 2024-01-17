use std::ops::ControlFlow;

use futures::future::BoxFuture;
use futures::FutureExt;
use mediatype::names::BOUNDARY;
use mediatype::ReadParams;
use multer::Multipart;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;

use crate::layers::async_checkpoint::OneShotAsyncCheckpointService;
use crate::layers::ServiceBuilderExt;
use crate::services::router;
use crate::services::router::ClientRequestContentType;
use crate::Configuration;

#[derive(Debug, Clone)]
pub(crate) struct ServiceLayer {
    pub(crate) allow_http_multipart: bool,
}

impl ServiceLayer {
    pub(crate) fn new(configuration: &Configuration) -> Self {
        Self {
            allow_http_multipart: configuration
                .experimental_file_uploads
                .protocols
                .restricted_multipart_request
                .is_some(),
        }
    }
}

impl<S> Layer<S> for ServiceLayer
where
    S: Service<router::Request, Response = router::Response, Error = BoxError> + Send + 'static,
    <S as Service<router::Request>>::Future: Send + 'static,
{
    type Service = OneShotAsyncCheckpointService<
        S,
        BoxFuture<'static, Result<ControlFlow<router::Response, router::Request>, BoxError>>,
        router::Request,
    >;

    fn layer(&self, service: S) -> Self::Service {
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: router::Request| {
                extract_operations(req)
                    .map(|req| Ok(ControlFlow::Continue(req)))
                    .boxed()
            })
            .service(service)
    }
}

async fn extract_operations(req: router::Request) -> router::Request {
    let content_type = req
        .context
        .private_entries
        .lock()
        .get::<ClientRequestContentType>()
        .cloned();

    match content_type {
        Some(ClientRequestContentType::MultipartFormData(mime)) => {
            // FIXME: remove unwrap, multer::Error::NoBoundary
            let boundary = mime.get_param(BOUNDARY).unwrap().as_str();
            let (request_parts, request_body) = req.router_request.into_parts();
            let mut multipart = Multipart::new(request_body, boundary);
            println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!service_layer {:?}", boundary);

            // FIXME: unwrap
            let operations_field = multipart.next_field().await.unwrap().unwrap();
            // FIXME
            assert!(
                operations_field.name() == Some("operations"),
                "Missing multipart field ‘operations’, please see GRAPHQL_MULTIPART_REQUEST_SPEC_URL.",
            );
            req.context
                .private_entries
                .lock()
                .insert(ServiceLayerResult { multipart });
            router::Request::from((
                http::Request::from_parts(
                    request_parts,
                    hyper::Body::wrap_stream(operations_field),
                ),
                req.context,
            ))
        }
        _ => req,
    }
}

pub(super) struct ServiceLayerResult {
    pub(super) multipart: Multipart<'static>,
}
