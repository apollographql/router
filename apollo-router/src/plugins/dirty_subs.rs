use futures::FutureExt;
use futures::StreamExt;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender;
use tower::BoxError;
use tower::ServiceExt;

use crate::graphql;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

// !!!!!!!!!!!!! To enable it provide a configuration like `dirty_subs: null` in config file

#[derive(Debug, Clone)]
struct DirtySubs;

struct DirtyRx(Sender<graphql::Response>);

#[async_trait::async_trait]
impl Plugin for DirtySubs {
    type Config = ();

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(DirtySubs {})
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        service
            .map_request(move |mut req: SubgraphRequest| {
                let (tx, rx) = oneshot::channel::<graphql::Response>();
                req.context.extensions().lock().insert(DirtyRx(tx));
                if let Some(sub_stream) = &req.subscription_stream {
                    let rx_stream = rx.into_stream().map(|r| match r {
                        Ok(r) => r,
                        Err(err) => graphql::Response::builder()
                            .error(
                                graphql::Error::builder()
                                    .message(format!("error rx stream: {:?}", err))
                                    .extension_code("DIRTY_ERROR")
                                    .build(),
                            )
                            .build(),
                    });
                    sub_stream.try_send(Box::pin(rx_stream)).unwrap();
                }

                req
            })
            .map_response(|res: SubgraphResponse| {
                let tx: DirtyRx = res.context.extensions().lock().remove().unwrap();

                tx.0.send(res.response.body().clone()).unwrap();
                res
            })
            .boxed()
    }
}

register_plugin!("apollo", "dirty_subs", DirtySubs);
