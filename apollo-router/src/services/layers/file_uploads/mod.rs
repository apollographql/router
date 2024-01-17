mod service_layer;
mod supergraph_layer;

pub(crate) use service_layer::ServiceLayer;
pub(crate) use supergraph_layer::SupergraphLayer;
// struct SupergraphLayer {}

// impl<S> Layer<S> for SupergraphLayer
// where
//     S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
//         + Send
//         + 'static,
//     <S as Service<supergraph::Request>>::Future: Send + 'static,
// {
//     type Service = supergraph::BoxService;

//     fn layer(&self, service: S) -> Self::Service {
//         service.boxed()
//     }
// }
