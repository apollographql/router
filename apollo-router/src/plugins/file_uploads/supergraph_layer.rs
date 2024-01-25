use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use multer::Multipart;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;

use crate::layers::async_checkpoint::OneShotAsyncCheckpointService;
use crate::layers::ServiceBuilderExt;
use crate::services::supergraph;
use crate::Configuration;

use super::service_layer::ServiceLayerResult;

#[derive(Debug, Clone)]
pub(crate) struct SupergraphLayer {}

impl SupergraphLayer {
    pub(crate) fn new(configuration: &Configuration) -> Self {
        Self {}
    }
}

impl<S> Layer<S> for SupergraphLayer
where
    S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
        + Send
        + 'static,
    <S as Service<supergraph::Request>>::Future: Send + 'static,
{
    type Service = OneShotAsyncCheckpointService<
        S,
        BoxFuture<
            'static,
            Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError>,
        >,
        supergraph::Request,
    >;

    fn layer(&self, service: S) -> Self::Service {
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: supergraph::Request| {
                extract_map(req)
                    .map(|req| Ok(ControlFlow::Continue(req)))
                    .boxed()
            })
            .service(service)
    }
}

async fn extract_map(req: supergraph::Request) -> supergraph::Request {
    let service_layer_result = req
        .context
        .private_entries
        .lock()
        .remove::<ServiceLayerResult>();
    match service_layer_result {
        Some(ServiceLayerResult { mut multipart }) => {
            // FIXME: unwrap
            let map_field = multipart.next_field().await.unwrap().unwrap();
            // FIXME
            assert!(
                map_field.name() == Some("map"),
                "Missing multipart field ‘map’, please see GRAPHQL_MULTIPART_REQUEST_SPEC_URL.",
            );
            // FIXME: apply some limit on size of map field
            let map_field = map_field.bytes().await.unwrap();
            // FIXME: unwrap
            let map_field: MapField = serde_json::from_slice(&map_field).unwrap();
            // FIXME: check number of files
            // assert!(map_field.len());
            println!("???????????????????????????????????????????{:?}", map_field);
            req.context
                .private_entries
                .lock()
                .insert(Arc::new(SupergraphLayerResult {
                    multipart,
                    map_field,
                }));
            req
        }
        _ => req,
    }
}

type MapField = HashMap<String, Vec<String>>;

pub(super) struct SupergraphLayerResult {
    pub(super) multipart: Multipart<'static>,
    pub(super) map_field: MapField,
}
