use std::ops::ControlFlow;

use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;

use crate::layers::ServiceBuilderExt;
use crate::layers::ServiceExt as _;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::CLIENT_LIBRARY_NAME;
use crate::plugins::telemetry::CLIENT_LIBRARY_VERSION;
use crate::plugins::telemetry::is_valid_client_library_value;
use crate::services::supergraph;

const CLIENT_LIBRARY_KEY: &str = "clientLibrary";
const CLIENT_LIBRARY_NAME_KEY: &str = "name";
const CLIENT_LIBRARY_VERSION_KEY: &str = "version";

/// The enhanced client-awareness plugin has no configuration.
#[derive(Debug, Deserialize, JsonSchema)]
struct Config {}

struct EnhancedClientAwareness {}

#[async_trait::async_trait]
impl Plugin for EnhancedClientAwareness {
    type Config = Config;

    // This is invoked once after the router starts and compiled-in
    // plugins are registered
    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(EnhancedClientAwareness {})
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneSyncService,
    ) -> supergraph::BoxCloneSyncService {
        ServiceBuilder::new()
            .checkpoint(|request: supergraph::Request| {
                if let Some(client_library_metadata) = request
                    .supergraph_request
                    .body()
                    .extensions
                    .get(CLIENT_LIBRARY_KEY)
                {
                    if let Some(client_library_name) = client_library_metadata
                        .get(CLIENT_LIBRARY_NAME_KEY)
                        .and_then(|value| value.as_str())
                    {
                        if !is_valid_client_library_value(client_library_name) {
                            tracing::warn!(
                                "Rejecting request: invalid client library name extension value"
                            );
                            return Ok(ControlFlow::Break(
                                supergraph::Response::error_builder()
                                    .status_code(StatusCode::BAD_REQUEST)
                                    .context(request.context)
                                    .build()?,
                            ));
                        }
                        let _ = request
                            .context
                            .insert(CLIENT_LIBRARY_NAME, client_library_name.to_string());
                    }

                    if let Some(client_library_version) = client_library_metadata
                        .get(CLIENT_LIBRARY_VERSION_KEY)
                        .and_then(|value| value.as_str())
                    {
                        if !is_valid_client_library_value(client_library_version) {
                            tracing::warn!(
                                "Rejecting request: invalid client library version extension value"
                            );
                            return Ok(ControlFlow::Break(
                                supergraph::Response::error_builder()
                                    .status_code(StatusCode::BAD_REQUEST)
                                    .context(request.context)
                                    .build()?,
                            ));
                        }
                        let _ = request
                            .context
                            .insert(CLIENT_LIBRARY_VERSION, client_library_version.to_string());
                    }
                }

                Ok(ControlFlow::Continue(request))
            })
            .service(service)
            .boxed_clone_sync()
    }
}

register_plugin!(
    "apollo",
    "enhanced_client_awareness",
    EnhancedClientAwareness
);

#[cfg(test)]
mod tests;
