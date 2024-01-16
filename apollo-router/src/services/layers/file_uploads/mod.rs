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

use crate::configuration::file_uploads::RestrictedMultipartRequest;
use crate::layers::async_checkpoint::OneShotAsyncCheckpointService;
use crate::layers::ServiceBuilderExt;
use crate::services::router;
use crate::services::router::ClientRequestContentType;
use crate::services::supergraph;
use crate::Configuration;

#[derive(Debug, Clone)]
pub(crate) struct FileUploadsLayer {
    config: Option<RestrictedMultipartRequest>,
}

impl FileUploadsLayer {
    pub(crate) fn new(configuration: &Configuration) -> Self {
        FileUploadsLayer {
            config: configuration
                .experimental_file_uploads
                .protocols
                .restricted_multipart_request
                .clone(),
        }
    }
    pub(crate) fn allow_http_multipart(&self) -> bool {
        self.config.is_some()
    }
}

impl<S> Layer<S> for FileUploadsLayer
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

async fn read_multipart_field<'a>(multipart: &mut Multipart<'a>, name: &str) -> multer::Field<'a> {
    // FIXME: unwrap
    let field = multipart.next_field().await.unwrap().unwrap();
    // FIXME
    assert!(
        field.name() == Some(name),
        "Missing multipart field ‘{}’, please see GRAPHQL_MULTIPART_REQUEST_SPEC_URL.",
        name
    );
    field
}

async fn extract_operations(req: router::Request) -> router::Request {
    let content_type: ClientRequestContentType =
        req.context.private_entries.lock().get().cloned().unwrap();
    match content_type {
        ClientRequestContentType::MultipartFormData(mime) => {
            // FIXME: remove unwrap, multer::Error::NoBoundary
            let boundary = mime.get_param(BOUNDARY).unwrap().as_str();
            let (request_parts, request_body) = req.router_request.into_parts();
            let mut multipart = Multipart::new(request_body, boundary);
            println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!{:?}", boundary);

            let operations = read_multipart_field(&mut multipart, "operations").await;
            // let map = read_multipart_field(&mut multipart, "map").await;
            // let map: DeserializeOwned = serde_json::from_slice(&map);

            router::Request::from((
                http::Request::from_parts(request_parts, hyper::Body::wrap_stream(operations)),
                req.context,
            ))
        }
        _ => req,
    }
}

// impl<S> Layer<S> for FileUploadsLayer
// where
//     S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
//         + Send
//         + 'static,
//     <S as Service<supergraph::Request>>::Future: Send + 'static,
// {
//     type Service = OneShotAsyncCheckpointService<
//         S,
//         BoxFuture<
//             'static,
//             Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError>,
//         >,
//         router::Request,
//     >;

//     fn layer(&self, service: S) -> Self::Service {}
// }
