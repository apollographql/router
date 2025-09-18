use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use tower::ServiceBuilder;
use tower::ServiceExt;

use super::subgraph::AuthConfig;
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
                if let Some(ref source_name) = req.connector.id.source_name
                    && let Some(signing_params) = signing_params
                        .get(&ConnectorSourceRef::new(
                            req.connector.id.subgraph_name.clone(),
                            source_name.clone(),
                        ))
                        .cloned()
                {
                    req.context
                        .extensions()
                        .with_lock(|lock| lock.insert(signing_params));
                }
                req
            })
            .service(service)
            .boxed()
    }
}

/// Configure connector authentication
#[derive(Clone, Debug, Default, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "AuthenticationConnectorConfig")]
pub(crate) struct Config {
    #[serde(default)]
    /// Create a configuration that will apply only to a specific source.
    pub(crate) sources: HashMap<String, AuthConfig>,
}
