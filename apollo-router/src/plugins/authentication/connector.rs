use std::collections::HashMap;
use std::sync::Arc;

use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::services::connector;
use crate::services::connector_service::ConnectorSourceRef;

pub(super) struct ConnectorAuth {
    pub(super) signing_params: Arc<HashMap<ConnectorSourceRef, Arc<SigningParamsConfig>>>,
}

impl ConnectorAuth {
    pub(super) fn connector_request_service(
        &self,
        service: connector::request_service::BoxService,
    ) -> connector::request_service::BoxService {
        let signing_params = self.signing_params.clone();
        ServiceBuilder::new()
            .map_request(move |req: connector::request_service::Request| {
                if let Some(ref source_name) = req.connector.id.source_name {
                    if let Some(signing_params) = signing_params
                        .get(&ConnectorSourceRef::new(
                            req.connector.id.subgraph_name.clone(),
                            source_name.clone(),
                        ))
                        .cloned()
                    {
                        req.context
                            .extensions()
                            .with_lock(|mut lock| lock.insert(signing_params));
                    }
                }
                req
            })
            .service(service)
            .boxed()
    }
}
