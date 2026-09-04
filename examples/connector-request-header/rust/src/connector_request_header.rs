use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_router::plugin::PluginInit;
use apollo_router::plugin::PluginUnstable;
use apollo_router::register_plugin;
use apollo_router::services::connector::request_service;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceExt;

// `connector_request_service` lets a plugin wrap the service that makes individual
// HTTP requests to Apollo Connectors -- the same hook a coprocessor reaches through
// the `ConnectorRequest`/`ConnectorResponse` stages, without the out-of-process hop.
//
// It's defined on `PluginUnstable` rather than the stable `Plugin` trait, so it may
// still change shape. Implementing `PluginUnstable` requires the extra
// `unstable_method` below with no default body -- that's deliberate, so opting into
// this hook is a visible choice in the plugin.
#[derive(Default)]
struct ConnectorRequestHeader {}

#[async_trait::async_trait]
impl PluginUnstable for ConnectorRequestHeader {
    // As with `Plugin`, config for this plugin (none needed here) is deserialized
    // from its section of `router.yaml` and passed to `new` as part of `PluginInit`.
    type Config = ();

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self::default())
    }

    // Called once per outbound connector request, not once per operation: a single
    // GraphQL operation can fan out into many connector requests.
    fn connector_request_service(
        &self,
        service: request_service::BoxService,
        _source_name: String,
    ) -> request_service::BoxService {
        BoxService::new(
            service.map_request(|mut request: request_service::Request| {
                // `Request::transport_request` carries the outbound `http::Request`, so
                // a plugin can read and rewrite its URI, headers, body and method.
                if let TransportRequest::Http(http_request) = &mut request.transport_request {
                    http_request
                        .inner
                        .headers_mut()
                        .insert("x-from-router-plugin", "yes".parse().unwrap());
                }
                request
            }),
        )
    }

    fn unstable_method(&self) {}
}

register_plugin!(
    "example",
    "connector_request_header",
    ConnectorRequestHeader
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// This test ensures the router will be able to find our `connector_request_header` plugin (if
    /// it hadn't, resolving `example.connector_request_header` from config would fail with an
    /// unknown-plugin error instead of building).
    #[tokio::test]
    async fn plugin_registered() {
        let config = json!({
            "plugins": {
                "example.connector_request_header": null
            }
        });
        apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }
}
