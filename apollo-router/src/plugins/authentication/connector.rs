use std::collections::HashMap;
use std::sync::Arc;

use tower::{ServiceBuilder, ServiceExt};
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::services::connector_service::{ConnectorInfo, ConnectorSourceRef, CONNECTOR_INFO_CONTEXT_KEY};
use crate::services::http::HttpRequest;

pub(super) struct ConnectorAuth {
    pub(super) signing_params: Arc<HashMap<ConnectorSourceRef, Arc<SigningParamsConfig>>>,
}

impl ConnectorAuth {
    pub(super) fn http_client_service(
        &self,
        subgraph_name: &str,
        service: crate::services::http::BoxService,
    ) -> crate::services::http::BoxService {
        let signing_params = self.signing_params.clone();
        let subgraph_name = subgraph_name.to_string();
        ServiceBuilder::new()
            .map_request(move |req: HttpRequest| {
                if let Ok(Some(connector_info)) = req
                    .context
                    .get::<&str, ConnectorInfo>(CONNECTOR_INFO_CONTEXT_KEY)
                {
                    if let Some(source_name) = connector_info.source_name {
                        if let Some(signing_params) = signing_params
                            .get(&ConnectorSourceRef::new(subgraph_name.clone(), source_name.clone()))
                            .cloned()
                        {
                            req.context
                                .extensions()
                                .with_lock(|mut lock| lock.insert(signing_params));
                        }
                    }
                }
                req
            })
            .service(service)
            .boxed()
    }
}