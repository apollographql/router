use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::CLIENT_LIBRARY_NAME;
use crate::plugins::telemetry::CLIENT_LIBRARY_VERSION;
use crate::services::supergraph;

const CLIENT_LIBRARY_KEY: &str = "clientLibrary";
const CLIENT_LIBRARY_NAME_KEY: &str = "name";
const CLIENT_LIBRARY_VERSION_KEY: &str = "version";

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

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
            .map_request(move |request: supergraph::Request| {
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
                        let _ = request
                            .context
                            .insert(CLIENT_LIBRARY_NAME, client_library_name.to_string());
                    };

                    if let Some(client_library_version) = client_library_metadata
                        .get(CLIENT_LIBRARY_VERSION_KEY)
                        .and_then(|value| value.as_str())
                    {
                        let _ = request
                            .context
                            .insert(CLIENT_LIBRARY_VERSION, client_library_version.to_string());
                    };
                };

                request
            })
            .service(service)
            .boxed()
    }
}

register_plugin!(
    "apollo",
    "enhanced_client_awareness",
    EnhancedClientAwareness
);

#[cfg(test)]
mod tests;
